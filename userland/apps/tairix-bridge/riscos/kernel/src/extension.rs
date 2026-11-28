//! State the session's other crates keep for their built-in modules: one
//! value of each type, which a module's initialisation holds and its
//! finalisation lets go of, as a RISC OS module claims its workspace. A value
//! is the session's or the current context's; a context's own are set aside
//! with it while another context runs, and given back when it ends.
//!
//! A session holds a handful, one or two for each crate, so a scan of their
//! types finds one faster than a map would.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::{Any, TypeId};

use crate::os::{Fail, Os, World};

/// What a context's end does with a value held for it: gives back what the
/// value has in the guest. It runs while the ending context is still
/// current, its other values already taken from it.
pub type End = for<'a, 'b> fn(&'a mut Os<'b>, Box<dyn Any>);

/// A value holding caches the machine's memory pressure takes back.
pub trait Reclaims: Any {
    /// What the pressure's band no longer allows the caches to keep, given
    /// back.
    fn reclaim(&mut self);
}

/// What a value is given at a context's end and when the pressure moves.
#[derive(Clone, Copy)]
struct Ways {
    end: Option<End>,
    reclaim: Option<fn(&mut dyn Any)>,
}

impl Ways {
    const PLAIN: Self = Self {
        end: None,
        reclaim: None,
    };
}

struct Held {
    id: TypeId,
    value: Box<dyn Any>,
    ways: Ways,
}

/// Values, each with its type.
pub(crate) struct Values(Vec<Held>);

impl Values {
    pub(crate) const fn new() -> Self {
        Self(Vec::new())
    }

    fn place<T: Any>(&self) -> Option<usize> {
        let id = TypeId::of::<T>();
        self.0.iter().position(|held| held.id == id)
    }

    fn get_mut<T: Any>(&mut self) -> Option<&mut T> {
        let place = self.place::<T>()?;
        self.0.get_mut(place)?.value.downcast_mut()
    }

    /// `value` held as the `T`, in place of any before: false where there
    /// is no room to hold it.
    fn hold<T: Any>(&mut self, value: Box<dyn Any>, ways: Ways) -> bool {
        let held = Held {
            id: TypeId::of::<T>(),
            value,
            ways,
        };
        if let Some(place) = self.place::<T>() {
            self.0[place] = held;
            return true;
        }
        if self.0.try_reserve(1).is_err() {
            return false;
        }
        self.0.push(held);
        true
    }

    /// The `T` taken out with its ways, a placeholder left that no lookup of
    /// a `T` finds.
    fn set_aside<T: Any>(&mut self) -> Option<(Box<T>, Ways)> {
        let place = self.place::<T>()?;
        let held = self.0.get_mut(place)?;
        match core::mem::replace(&mut held.value, Box::new(())).downcast::<T>() {
            Ok(value) => Some((value, held.ways)),
            Err(other) => {
                held.value = other;
                None
            }
        }
    }

    /// Each value holding caches told the pressure moved.
    fn reclaim(&mut self) {
        for held in &mut self.0 {
            if let Some(reclaim) = held.ways.reclaim {
                reclaim(&mut *held.value);
            }
        }
    }

    fn remove<T: Any>(&mut self) -> Option<T> {
        let place = self.place::<T>()?;
        let held = self.0.swap_remove(place);
        held.value.downcast().ok().map(|boxed| *boxed)
    }

    /// Each value given to its end, the values with none dropped.
    pub(crate) fn end(self, os: &mut Os<'_>) {
        for held in self.0 {
            if let Some(end) = held.ways.end {
                end(os, held.value);
            }
        }
    }
}

/// Which values hold a type.
#[derive(Clone, Copy)]
enum Holder {
    Session,
    Context,
}

/// The session's values, and the current context's.
pub(crate) struct Extensions {
    session: Values,
    context: Values,
}

impl Extensions {
    pub(crate) const fn new() -> Self {
        Self {
            session: Values::new(),
            context: Values::new(),
        }
    }

    fn holder<T: Any>(&self) -> Option<Holder> {
        if self.session.place::<T>().is_some() {
            Some(Holder::Session)
        } else {
            self.context.place::<T>().map(|_| Holder::Context)
        }
    }

    const fn values(&mut self, holder: Holder) -> &mut Values {
        match holder {
            Holder::Session => &mut self.session,
            Holder::Context => &mut self.context,
        }
    }

    pub(crate) fn get_mut<T: Any>(&mut self) -> Option<&mut T> {
        let holder = self.holder::<T>()?;
        self.values(holder).get_mut()
    }

    /// The current context's values exchanged for `other`'s, as a switch
    /// sets one context aside for another.
    pub(crate) const fn exchange_context(&mut self, other: &mut Values) {
        core::mem::swap(&mut self.context, other);
    }
}

impl World {
    /// The value of type `T` the session or the current context holds.
    pub fn extension<T: Any>(&mut self) -> Option<&mut T> {
        self.extensions.get_mut()
    }
}

impl Os<'_> {
    /// The value of type `T` the session or the current context holds.
    pub fn extension<T: Any>(&mut self) -> Option<&mut T> {
        self.world.extension()
    }

    /// Holds `value` as the session's `T`, in place of any held before.
    ///
    /// # Errors
    ///
    /// "Not enough memory" where the host has no room for it.
    pub fn hold_extension<T: Any>(&mut self, value: T) -> Result<(), Fail> {
        self.hold_in(Holder::Session, value, Ways::PLAIN)
    }

    /// Holds `value` as the session's `T`, as [`Self::hold_extension`] does,
    /// its caches given back as [`Self::pressure_moved`] tells it.
    ///
    /// # Errors
    ///
    /// "Not enough memory" where the host has no room for it.
    pub fn hold_cache_extension<T: Reclaims>(&mut self, value: T) -> Result<(), Fail> {
        let reclaim: fn(&mut dyn Any) = |value| {
            if let Some(value) = value.downcast_mut::<T>() {
                value.reclaim();
            }
        };
        let ways = Ways {
            end: None,
            reclaim: Some(reclaim),
        };
        self.hold_in(Holder::Session, value, ways)
    }

    /// Holds `value` as the current context's `T`, in place of any it held
    /// before: each context has its own, which another context's work never
    /// finds, and `end` is given it when the context ends.
    ///
    /// # Errors
    ///
    /// "Not enough memory" where the host has no room for it.
    pub fn hold_context_extension<T: Any>(&mut self, value: T, end: End) -> Result<(), Fail> {
        let ways = Ways {
            end: Some(end),
            reclaim: None,
        };
        self.hold_in(Holder::Context, value, ways)
    }

    fn hold_in<T: Any>(&mut self, holder: Holder, value: T, ways: Ways) -> Result<(), Fail> {
        let Ok(boxed) = tairix_collections::try_box(value) else {
            return Err(self.no_memory());
        };
        if self.world.extensions.values(holder).hold::<T>(boxed, ways) {
            Ok(())
        } else {
            Err(self.no_memory())
        }
    }

    /// The machine's memory pressure moved: every value the session holds
    /// caches in gives back what the band no longer allows.
    pub fn pressure_moved(&mut self) {
        self.world.extensions.session.reclaim();
    }

    /// `work` on the `T` held and the rest of the session together, while a
    /// `T` is held. The value is set aside meanwhile, so nothing `work`
    /// calls finds it, and `work` must neither hold nor release a `T`.
    pub fn with_extension<T: Any, R>(
        &mut self,
        work: impl FnOnce(&mut T, &mut Self) -> R,
    ) -> Option<R> {
        let holder = self.world.extensions.holder::<T>()?;
        let (mut value, ways) = self.world.extensions.values(holder).set_aside::<T>()?;
        let answer = work(&mut value, self);
        // Only `work` letting go of its own `T` empties the place; the value
        // set aside is its holder's again.
        let _ = self.world.extensions.values(holder).hold::<T>(value, ways);
        Some(answer)
    }

    /// Lets go of the session's `T`, or of every context's, handing back the
    /// session's or the current context's. Another context's is dropped
    /// without its end, as the module that held it is going.
    pub fn release_extension<T: Any>(&mut self) -> Option<T> {
        if let Some(value) = self.world.extensions.session.remove::<T>() {
            return Some(value);
        }
        for parked in self.world.contexts.parked_extensions() {
            drop(parked.remove::<T>());
        }
        self.world.extensions.context.remove::<T>()
    }
}

#[cfg(test)]
mod tests;
