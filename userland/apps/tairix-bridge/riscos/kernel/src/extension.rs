//! State the session's other crates keep for their built-in modules: one
//! value of each type, which a module's initialisation holds and its
//! finalisation lets go of, as a RISC OS module claims its workspace.
//!
//! A session holds a handful, one or two for each crate, so a scan of their
//! types finds one faster than a map would.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::{Any, TypeId};

use crate::os::{Fail, Os, World};

/// The values, each with its type.
pub(crate) struct Extensions {
    values: Vec<(TypeId, Box<dyn Any>)>,
}

impl Extensions {
    pub(crate) const fn new() -> Self {
        Self { values: Vec::new() }
    }

    fn place<T: Any>(&self) -> Option<usize> {
        let id = TypeId::of::<T>();
        self.values.iter().position(|(held, _)| *held == id)
    }

    pub(crate) fn get_mut<T: Any>(&mut self) -> Option<&mut T> {
        let place = self.place::<T>()?;
        self.values.get_mut(place)?.1.downcast_mut()
    }
}

impl World {
    /// The session's value of type `T`, while one is held.
    pub fn extension<T: Any>(&mut self) -> Option<&mut T> {
        self.extensions.get_mut()
    }
}

impl Os<'_> {
    /// The session's value of type `T`, while one is held.
    pub fn extension<T: Any>(&mut self) -> Option<&mut T> {
        self.world.extension()
    }

    /// Holds `value` as the session's `T`, in place of any held before.
    ///
    /// # Errors
    ///
    /// "Not enough memory" where the host has no room for it.
    pub fn hold_extension<T: Any>(&mut self, value: T) -> Result<(), Fail> {
        let Ok(boxed) = tairix_collections::try_box(value) else {
            return Err(self.no_memory());
        };
        let extensions = &mut self.world.extensions;
        if let Some(slot) = extensions
            .place::<T>()
            .and_then(|place| extensions.values.get_mut(place))
        {
            slot.1 = boxed;
            return Ok(());
        }
        if extensions.values.try_reserve(1).is_err() {
            return Err(self.no_memory());
        }
        extensions.values.push((TypeId::of::<T>(), boxed));
        Ok(())
    }

    /// Lets go of the session's `T`, handing it back.
    pub fn release_extension<T: Any>(&mut self) -> Option<T> {
        let extensions = &mut self.world.extensions;
        let place = extensions.place::<T>()?;
        let (_, value) = extensions.values.swap_remove(place);
        value.downcast().ok().map(|boxed| *boxed)
    }
}

#[cfg(test)]
mod tests;
