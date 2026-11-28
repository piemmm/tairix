//! What a screen's window gives its context (`plans/RISCOS-EMULATOR.md`
//! §15.3): each key's edges by RISC OS low-level key number, the buttons in
//! the order the setting chooses, and the pointer as a place in OS units, or
//! as motion once the guest has set the pointer itself.

use tairix_abi::input::{PhysicalKey, PointerButtonCode};
use tairix_raster::resample::Region;
use tairix_riscos_kernel::input::{buttons, low_level, InputEvent};
use tairix_riscos_session::protocol::Screen;

/// Which of the desktop's buttons is which of RISC OS's.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonOrder {
    /// By the desktop's roles: the primary button Select, the secondary Menu
    /// and the middle Adjust.
    #[default]
    Roles,
    /// In a RISC OS mouse's order across: the primary button Select, the
    /// middle Menu and the secondary Adjust.
    Mouse,
}

impl ButtonOrder {
    /// The RISC OS button `code` is.
    #[must_use]
    pub const fn button(self, code: PointerButtonCode) -> u32 {
        match (self, code) {
            (_, PointerButtonCode::Primary) => buttons::SELECT,
            (Self::Roles, PointerButtonCode::Secondary)
            | (Self::Mouse, PointerButtonCode::Middle) => buttons::MENU,
            (Self::Roles, PointerButtonCode::Middle)
            | (Self::Mouse, PointerButtonCode::Secondary) => buttons::ADJUST,
        }
    }
}

/// The keys and buttons a screen's window holds down.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Held {
    /// By low-level key number.
    keys: u128,
    buttons: u32,
}

impl Held {
    /// A key's edge: its transition, none for a key with no low-level number,
    /// a press of a key already down or a release of one that is not.
    pub fn key(&mut self, key: PhysicalKey, down: bool) -> Option<InputEvent> {
        let number = low_level(key)?;
        let bit = 1u128.checked_shl(number)?;
        if (self.keys & bit != 0) == down {
            return None;
        }
        self.keys ^= bit;
        Some(InputEvent::Transition { key: number, down })
    }

    /// A button's edge, `order` naming which it is: the buttons held after.
    pub fn button(&mut self, order: ButtonOrder, code: PointerButtonCode, down: bool) -> u32 {
        let bit = order.button(code);
        if down {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        self.buttons
    }

    /// The buttons held.
    #[must_use]
    pub const fn buttons(&self) -> u32 {
        self.buttons
    }

    /// Every button let go: whether any was held.
    pub fn let_go_buttons(&mut self) -> bool {
        core::mem::take(&mut self.buttons) != 0
    }

    /// Everything held let go, as when the window loses the keyboard: each
    /// key's release, and whether any button was held.
    pub fn release(&mut self) -> (impl Iterator<Item = InputEvent>, bool) {
        let keys = core::mem::take(&mut self.keys);
        let buttons = self.let_go_buttons();
        let released = (0..u128::BITS).filter(move |&key| keys & (1 << key) != 0);
        (
            released.map(|key| InputEvent::Transition { key, down: false }),
            buttons,
        )
    }
}

/// The place, in OS units from the bottom left of `screen`, of the window's
/// pixel `(x, y)` over its picture scaled into `placement`: held to the
/// screen's edges.
#[must_use]
pub fn os_units(screen: &Screen, placement: Region, (x, y): (u32, u32)) -> (i32, i32) {
    let pixel = |at: u32, start: u32, span: u32, pixels: u32| {
        let into = u64::from(at.saturating_sub(start)).min(u64::from(span.max(1) - 1));
        let scaled = into * u64::from(pixels) / u64::from(span.max(1));
        u32::try_from(scaled).unwrap_or(u32::MAX).min(pixels - 1)
    };
    let across = pixel(x, placement.x, placement.width, screen.width);
    let down = pixel(y, placement.y, placement.height, screen.height);
    let up = screen.height - 1 - down;
    let units = |pixel: u32, eig: u32| i32::try_from(pixel << eig).unwrap_or(i32::MAX);
    (units(across, screen.x_eig), units(up, screen.y_eig))
}

/// How a screen's pointer reaches its guest: as a place until the guest sets
/// the pointer itself, then, once the window holds the pointer locked, as
/// motion.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Pointing {
    /// The guest has set the pointer or the mouse itself.
    set: bool,
    locked: bool,
}

impl Pointing {
    /// The guest set the pointer or the mouse.
    pub const fn guest_set(&mut self) {
        self.set = true;
    }

    /// The window took pointer lock, or lost it: on losing it, the release of
    /// every button `held` held, which the desktop leaves the window to let go
    /// of, told as the lock's motion so the pointer stays where the guest put
    /// it.
    pub fn locked(&mut self, locked: bool, held: &mut Held) -> Option<InputEvent> {
        let ended = self.locked && !locked;
        self.locked = locked;
        (ended && held.let_go_buttons()).then(|| motion(0, 0, 0))
    }

    /// Whether the pointer is locked to the window.
    #[must_use]
    pub const fn is_locked(&self) -> bool {
        self.locked
    }

    /// The event the pointer at `place` in OS units, `buttons` held, gives:
    /// none while it is locked, its motion going instead.
    #[must_use]
    pub const fn at(&self, (x, y): (i32, i32), buttons: u32) -> Option<InputEvent> {
        if self.locked {
            None
        } else {
            Some(InputEvent::Pointer { x, y, buttons })
        }
    }

    /// The event the buttons changing to `buttons` gives, the pointer at
    /// `place`; and whether the window should now ask for pointer lock: the
    /// guest drives the mouse itself and the primary button came up over the
    /// picture.
    #[must_use]
    pub const fn buttons(
        &self,
        place: (i32, i32),
        buttons: u32,
        primary_up: bool,
    ) -> (InputEvent, bool) {
        if self.locked {
            return (motion(0, 0, buttons), false);
        }
        let (x, y) = place;
        (
            InputEvent::Pointer { x, y, buttons },
            self.set && primary_up,
        )
    }

    /// The event the locked pointer's motion by `(dx, dy)` device counts
    /// gives, `buttons` held: none unless it is locked.
    #[must_use]
    pub const fn moved(&self, (dx, dy): (i32, i32), buttons: u32) -> Option<InputEvent> {
        if self.locked {
            Some(motion(dx, dy, buttons))
        } else {
            None
        }
    }
}

/// A locked pointer's motion as the guest takes it: the device's counts,
/// up rather than down.
#[must_use]
pub const fn motion(dx: i32, dy: i32, buttons: u32) -> InputEvent {
    InputEvent::Motion {
        dx,
        dy: dy.saturating_neg(),
        buttons,
    }
}

#[cfg(test)]
mod tests;
