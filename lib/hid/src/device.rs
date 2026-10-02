//! The HID device engine: one device's applications, configured through its
//! transport, fed its input reports, and answered as seat records.

use alloc::vec::Vec;

use tairix_abi::touch::TouchSurface;
use tairix_abi::DriverError;

use crate::config::{self, HidTransport};
use crate::descriptor::{CollectionIndex, CollectionKind, ReportDescriptor, ReportKind};
use crate::keyboard::KeyboardDecoder;
use crate::mouse::MouseDecoder;
use crate::touch::TouchDecoder;
use crate::usages::{
    DEVICE_CONFIGURATION, INPUT_MODE_TOUCHPAD, INPUT_MODE_TOUCHSCREEN, KEYBOARD, KEYPAD, MOUSE,
    POINTER, TOUCH_PAD, TOUCH_SCREEN,
};
use crate::{try_push, Decoded, SeatSink};

/// The applications a device was found to carry, for its driver's log.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Applications {
    /// Keyboards and keypads.
    pub keyboards: usize,
    /// Relative pointers.
    pub mice: usize,
    /// Touch pads, clickpads among them.
    pub touchpads: usize,
    /// Touch screens.
    pub touchscreens: usize,
}

/// One HID device.
#[derive(Debug)]
pub struct HidDevice {
    model: ReportDescriptor,
    keyboards: Vec<KeyboardDecoder>,
    mice: Vec<MouseDecoder>,
    touches: Vec<(CollectionIndex, TouchDecoder)>,
    configurations: Vec<CollectionIndex>,
}

impl HidDevice {
    /// The engine for a device whose report descriptor parsed into `model`;
    /// `None` when it carries no application the seat serves, or memory for
    /// the decoders runs out.
    #[must_use]
    pub fn new(model: ReportDescriptor) -> Option<Self> {
        let mut device = Self {
            model,
            keyboards: Vec::new(),
            mice: Vec::new(),
            touches: Vec::new(),
            configurations: Vec::new(),
        };
        let mut touch_devices = 0u16;
        for (index, collection) in device.model.collections().iter().enumerate() {
            if collection.parent.is_some() || collection.kind != CollectionKind::Application {
                continue;
            }
            let at = CollectionIndex::new(index)?;
            match collection.usage {
                KEYBOARD | KEYPAD => {
                    if let Some(decoder) = KeyboardDecoder::new(&device.model, at) {
                        try_push(&mut device.keyboards, decoder)?;
                    }
                }
                MOUSE | POINTER => {
                    if let Some(decoder) = MouseDecoder::new(&device.model, at) {
                        try_push(&mut device.mice, decoder)?;
                    }
                }
                TOUCH_PAD | TOUCH_SCREEN => {
                    let screen = collection.usage == TOUCH_SCREEN;
                    if let Some(decoder) =
                        TouchDecoder::new(&device.model, at, screen, touch_devices)
                    {
                        try_push(&mut device.touches, (at, decoder))?;
                        touch_devices += 1;
                    }
                }
                DEVICE_CONFIGURATION => try_push(&mut device.configurations, at)?,
                _ => {}
            }
        }
        let serves =
            !device.keyboards.is_empty() || !device.mice.is_empty() || !device.touches.is_empty();
        serves.then_some(device)
    }

    /// The parsed report descriptor.
    #[must_use]
    pub const fn model(&self) -> &ReportDescriptor {
        &self.model
    }

    /// The applications the device carries.
    #[must_use]
    pub fn applications(&self) -> Applications {
        let screens = self
            .touches
            .iter()
            .filter(|(_, touch)| touch.surface() == TouchSurface::Screen)
            .count();
        Applications {
            keyboards: self.keyboards.len(),
            mice: self.mice.len(),
            touchpads: self.touches.len() - screens,
            touchscreens: screens,
        }
    }

    /// The longest input report the device sends, in bytes.
    #[must_use]
    pub fn longest_input(&self) -> usize {
        self.model.longest_report(ReportKind::Input)
    }

    /// Configure the device: a digitizer switched to report contacts, its
    /// limits read, each wheel raised to its finest resolution.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] when the device went while being asked; a
    /// feature it refuses is left as it is.
    pub fn configure(&mut self, transport: &mut dyn HidTransport) -> Result<(), DriverError> {
        let applications = self.applications();
        let mode = if applications.touchpads > 0 {
            Some(INPUT_MODE_TOUCHPAD)
        } else if applications.touchscreens > 0 {
            Some(INPUT_MODE_TOUCHSCREEN)
        } else {
            None
        };
        if let Some(mode) = mode {
            config::input_mode(&self.model, transport, &self.configurations, mode)?;
        }
        for (application, touch) in &mut self.touches {
            config::touch_limits(&self.model, transport, *application, touch)?;
        }
        for mouse in &mut self.mice {
            config::wheel_resolution(&self.model, transport, mouse)?;
        }
        Ok(())
    }

    /// Decode one input report onto `sink`: [`Decoded::Malformed`] when an
    /// application it belongs to finds it cut short, [`Decoded::NotMine`]
    /// when none does.
    ///
    /// # Errors
    ///
    /// What `sink` refuses.
    pub fn input(
        &mut self,
        report: &[u8],
        sink: &mut dyn SeatSink,
    ) -> Result<Decoded, DriverError> {
        let mut outcome = Decoded::NotMine;
        for keyboard in &mut self.keyboards {
            outcome = outcome.and(keyboard.decode(&self.model, report, sink)?);
        }
        for mouse in &mut self.mice {
            outcome = outcome.and(mouse.decode(&self.model, report, sink)?);
        }
        for (_, touch) in &mut self.touches {
            outcome = outcome.and(touch.decode(&self.model, report, sink)?);
        }
        Ok(outcome)
    }

    /// Release every key, button and contact the device holds: what a
    /// device that has gone leaves behind.
    ///
    /// # Errors
    ///
    /// The first refusal of `sink`; every application is released all the
    /// same.
    pub fn release(&mut self, sink: &mut dyn SeatSink) -> Result<(), DriverError> {
        let mut first = Ok(());
        let mut keep = |result: Result<(), DriverError>| {
            if first.is_ok() {
                first = result;
            }
        };
        for keyboard in &mut self.keyboards {
            keep(keyboard.release(sink));
        }
        for mouse in &mut self.mice {
            keep(mouse.release(sink));
        }
        for (_, touch) in &mut self.touches {
            keep(touch.release(sink));
        }
        first
    }
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
