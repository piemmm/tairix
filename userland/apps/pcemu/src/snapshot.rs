//! A machine's whole state as bytes, for a machine suspended and resumed:
//! each part writes what it keeps, in order, and takes it back into a part
//! built from the same configuration, every value checked as it is read.
//!
//! `state!` destructures a struct whole, so every field is either kept or
//! named as left as built: a field added without saying which does not
//! compile. What a part holds only because of its configuration — a model,
//! a size, a table worked out from them — is left as built, and the length of
//! every buffer must be the one built, so a snapshot of a machine configured
//! otherwise is refused rather than read into the wrong shape.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_sandbox::wire::{Reader, Refusable, WireError, Writer};

/// Where a snapshot is written: refused whole once the heap refuses any of it.
pub type Out = Writer<Refusable>;

/// A snapshot being read.
pub type In<'a> = Reader<'a>;

/// Why a snapshot was not taken back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Torn {
    /// It ended early, or held a value no part could have.
    Malformed,
    /// It was taken of a machine built otherwise.
    Mismatch,
}

impl From<WireError> for Torn {
    fn from(_: WireError) -> Self {
        Self::Malformed
    }
}

/// A part of the machine a snapshot keeps.
pub trait State {
    /// Write what the part keeps.
    fn save(&self, out: &mut Out);

    /// Take back what [`save`](Self::save) wrote, into a part built from the
    /// same configuration.
    ///
    /// # Errors
    ///
    /// [`Torn`] for a value the part could not hold. The part is then partly
    /// restored, and the machine it belongs to is built again.
    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn>;
}

/// `Ok` where `holds`, else [`Torn::Malformed`]: a value restored that breaks
/// what its part relies on.
///
/// # Errors
///
/// [`Torn::Malformed`] where it does not hold.
pub const fn check(holds: bool) -> Result<(), Torn> {
    if holds {
        Ok(())
    } else {
        Err(Torn::Malformed)
    }
}

/// Write a part fitted or not as its configuration says.
pub fn save_fitted<T: State>(part: Option<&T>, out: &mut Out) {
    match part {
        Some(part) => {
            out.u8(1);
            part.save(out);
        }
        None => out.u8(0),
    }
}

/// Take back a part fitted or not as its configuration says.
///
/// # Errors
///
/// [`Torn::Mismatch`] where the snapshot's machine had it and this one does
/// not, or the other way about; what the part refuses.
pub fn restore_fitted<T: State>(part: Option<&mut T>, input: &mut In<'_>) -> Result<(), Torn> {
    match (input.u8()?, part) {
        (0, None) => Ok(()),
        (1, Some(part)) => part.restore(input),
        (0 | 1, _) => Err(Torn::Mismatch),
        _ => Err(Torn::Malformed),
    }
}

/// Take back an optional value [`State`] wrote for an `Option`, whose type
/// has no default: `make` builds the one a present value is restored into.
///
/// # Errors
///
/// [`Torn::Malformed`] for a tag that is neither; what the value refuses.
pub fn restore_option<T: State>(
    value: &mut Option<T>,
    make: impl FnOnce() -> T,
    input: &mut In<'_>,
) -> Result<(), Torn> {
    match input.u8()? {
        0 => *value = None,
        1 => value.get_or_insert_with(make).restore(input)?,
        _ => return Err(Torn::Malformed),
    }
    Ok(())
}

/// Write `bytes`, a buffer whose length the configuration sets.
pub fn save_bytes(bytes: &[u8], out: &mut Out) {
    out.bytes(bytes);
}

/// Take back a buffer [`save_bytes`] wrote into `bytes`, which must be the
/// length it was.
///
/// # Errors
///
/// [`Torn::Mismatch`] for another length; [`Torn::Malformed`] for one cut
/// short.
pub fn restore_bytes(bytes: &mut [u8], input: &mut In<'_>) -> Result<(), Torn> {
    let kept = input.bytes(bytes.len()).map_err(|_| Torn::Mismatch)?;
    if kept.len() != bytes.len() {
        return Err(Torn::Mismatch);
    }
    bytes.copy_from_slice(kept);
    Ok(())
}

/// Write the items of a queue that holds at most a bound of them.
pub fn save_queue<T: State>(queue: &VecDeque<T>, out: &mut Out) {
    out.u32(u32::try_from(queue.len()).unwrap_or(u32::MAX));
    for item in queue {
        item.save(out);
    }
}

/// Take back a queue [`save_queue`] wrote, of at most `most` items.
///
/// # Errors
///
/// [`Torn::Malformed`] for more items than that, or any item its type
/// refuses; [`Torn::Mismatch`] where the heap will not hold them.
pub fn restore_queue<T: State + Default>(
    queue: &mut VecDeque<T>,
    most: usize,
    input: &mut In<'_>,
) -> Result<(), Torn> {
    let len = usize::try_from(input.u32()?).map_err(|_| Torn::Malformed)?;
    check(len <= most)?;
    queue.clear();
    queue.try_reserve(len).map_err(|_| Torn::Mismatch)?;
    for _ in 0..len {
        let mut item = T::default();
        item.restore(input)?;
        queue.push_back(item);
    }
    Ok(())
}

macro_rules! scalar {
    ($($ty:ty => $put:ident, $get:ident;)*) => {
        $(
            impl State for $ty {
                fn save(&self, out: &mut Out) {
                    out.$put(*self);
                }

                fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
                    *self = input.$get()?;
                    Ok(())
                }
            }
        )*
    };
}

scalar! {
    u8 => u8, u8;
    u16 => u16, u16;
    u32 => u32, u32;
    u64 => u64, u64;
    i32 => i32, i32;
}

impl State for i8 {
    fn save(&self, out: &mut Out) {
        out.u8(self.cast_unsigned());
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        *self = input.u8()?.cast_signed();
        Ok(())
    }
}

impl State for i16 {
    fn save(&self, out: &mut Out) {
        out.u16(self.cast_unsigned());
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        *self = input.u16()?.cast_signed();
        Ok(())
    }
}

impl State for i64 {
    fn save(&self, out: &mut Out) {
        out.u64(self.cast_unsigned());
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        *self = input.u64()?.cast_signed();
        Ok(())
    }
}

impl State for usize {
    fn save(&self, out: &mut Out) {
        out.u64(u64::try_from(*self).unwrap_or(u64::MAX));
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        *self = usize::try_from(input.u64()?).map_err(|_| Torn::Malformed)?;
        Ok(())
    }
}

impl State for f32 {
    fn save(&self, out: &mut Out) {
        out.u32(self.to_bits());
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        *self = f32::from_bits(input.u32()?);
        Ok(())
    }
}

impl State for f64 {
    fn save(&self, out: &mut Out) {
        out.u64(self.to_bits());
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        *self = f64::from_bits(input.u64()?);
        Ok(())
    }
}

impl State for bool {
    fn save(&self, out: &mut Out) {
        out.u8(u8::from(*self));
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        *self = match input.u8()? {
            0 => false,
            1 => true,
            _ => return Err(Torn::Malformed),
        };
        Ok(())
    }
}

impl<T: State, const N: usize> State for [T; N] {
    fn save(&self, out: &mut Out) {
        for item in self {
            item.save(out);
        }
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        for item in self {
            item.restore(input)?;
        }
        Ok(())
    }
}

/// A value present or not as the machine ran; a part fitted or not by its
/// configuration is [`save_fitted`]'s.
impl<T: State + Default> State for Option<T> {
    fn save(&self, out: &mut Out) {
        save_fitted(self.as_ref(), out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        match input.u8()? {
            0 => *self = None,
            1 => self.get_or_insert_with(T::default).restore(input)?,
            _ => return Err(Torn::Malformed),
        }
        Ok(())
    }
}

/// A buffer of bytes the configuration sizes: restored only into one of
/// that length.
impl State for Vec<u8> {
    fn save(&self, out: &mut Out) {
        save_bytes(self, out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        restore_bytes(self, input)
    }
}

macro_rules! sized_buffer {
    ($($item:ty),*) => {
        $(
            /// A buffer the configuration sizes: restored only into one of
            /// that length.
            impl State for Vec<$item> {
                fn save(&self, out: &mut Out) {
                    out.u32(u32::try_from(self.len()).unwrap_or(u32::MAX));
                    for item in self {
                        item.save(out);
                    }
                }

                fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
                    if usize::try_from(input.u32()?).ok() != Some(self.len()) {
                        return Err(Torn::Mismatch);
                    }
                    for item in self {
                        item.restore(input)?;
                    }
                    Ok(())
                }
            }
        )*
    };
}

sized_buffer!(u16, u32, i16, i32, f32);

impl<A: State, B: State> State for (A, B) {
    fn save(&self, out: &mut Out) {
        self.0.save(out);
        self.1.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        self.0.restore(input)?;
        self.1.restore(input)
    }
}

impl<A: State, B: State, C: State> State for (A, B, C) {
    fn save(&self, out: &mut Out) {
        self.0.save(out);
        self.1.save(out);
        self.2.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        self.0.restore(input)?;
        self.1.restore(input)?;
        self.2.restore(input)
    }
}

/// [`State`] for a struct: every field kept, in order, except those named
/// after `as built`, which are left as the part was built; `check` names a
/// function the restored part must satisfy.
macro_rules! state {
    (
        $ty:ty { $($field:ident),* $(,)? }
        $(as built { $($kept:ident),* $(,)? })?
        $(check $check:expr)?
    ) => {
        impl $crate::snapshot::State for $ty {
            fn save(&self, out: &mut $crate::snapshot::Out) {
                let Self { $($field,)* $($($kept: _,)*)? } = self;
                $($crate::snapshot::State::save($field, out);)*
            }

            fn restore(
                &mut self,
                input: &mut $crate::snapshot::In<'_>,
            ) -> Result<(), $crate::snapshot::Torn> {
                let Self { $($field,)* $($($kept: _,)*)? } = self;
                $($crate::snapshot::State::restore($field, input)?;)*
                $($crate::snapshot::check(($check)(&*self))?;)?
                Ok(())
            }
        }
    };
}

/// [`State`] for an enum, each variant written as its tag and then its
/// fields; a field of a variant restored is restored from its default.
macro_rules! state_enum {
    (
        $ty:ty {
            $($tag:literal => $variant:ident
                $(( $($tuple:ident),* ))?
                $({ $($named:ident),* })?
            ),* $(,)?
        }
    ) => {
        impl $crate::snapshot::State for $ty {
            fn save(&self, out: &mut $crate::snapshot::Out) {
                match self {
                    $(Self::$variant $(( $($tuple),* ))? $({ $($named),* })? => {
                        out.u8($tag);
                        $($($crate::snapshot::State::save($tuple, out);)*)?
                        $($($crate::snapshot::State::save($named, out);)*)?
                    })*
                }
            }

            fn restore(
                &mut self,
                input: &mut $crate::snapshot::In<'_>,
            ) -> Result<(), $crate::snapshot::Torn> {
                match input.u8()? {
                    $($tag => {
                        $($(
                            let mut $tuple = Default::default();
                            $crate::snapshot::State::restore(&mut $tuple, input)?;
                        )*)?
                        $($(
                            let mut $named = Default::default();
                            $crate::snapshot::State::restore(&mut $named, input)?;
                        )*)?
                        *self = Self::$variant $(( $($tuple),* ))? $({ $($named),* })?;
                    })*
                    _ => return Err($crate::snapshot::Torn::Malformed),
                }
                Ok(())
            }
        }
    };
}

pub(crate) use {state, state_enum};

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
