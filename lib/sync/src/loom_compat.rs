//! The seam that lets the primitives be model-checked with `loom`.
//!
//! Under `--cfg loom` the atomics and cell below are loom's, which schedule
//! every interleaving for the model checker. A primitive whose orderings the
//! model must check reaches its atomics through here; one taken from
//! `core::sync::atomic` bypasses the model, as `FnCell`'s slot and the lock
//! diagnostics' owner word do on purpose.
//!
//! Loom's atomics have no `const` constructor and belong to one model
//! execution, so [`global!`](crate::global) and [`const_fn!`](crate::const_fn)
//! carry that to the statics and constructors of every crate built this way.

// Re-exports under `cfg(loom)`.
#[cfg(loom)]
pub(crate) use loom::hint::spin_loop;
#[cfg(loom)]
pub(crate) use loom::sync::atomic::{
    fence, AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering,
};

// Re-exports under normal (non-`loom`) builds.
#[cfg(not(loom))]
pub(crate) use core::hint::spin_loop;
#[cfg(not(loom))]
pub(crate) use core::sync::atomic::{
    fence, AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering,
};

/// A `Sync`-friendly `UnsafeCell` wrapper that mirrors `loom::cell::UnsafeCell`'s
/// `with`/`with_mut` access pattern.
///
/// The `with`/`with_mut` API is what `loom` uses to track pointer accesses
/// for race detection. By going through it unconditionally we get the same
/// access pattern in production code (which is enforced by the borrow
/// checker via the closure) and in `loom` runs (which gets the bookkeeping
/// it needs).
pub(crate) struct SyncUnsafeCell<T: ?Sized> {
    #[cfg(loom)]
    inner: loom::cell::UnsafeCell<T>,
    #[cfg(not(loom))]
    inner: core::cell::UnsafeCell<T>,
}

#[cfg(not(loom))]
impl<T> SyncUnsafeCell<T> {
    /// Constructs a new cell. Available as `const fn` in production builds
    /// so primitives can be used to back `static` globals.
    pub(crate) const fn new(value: T) -> Self {
        Self {
            inner: core::cell::UnsafeCell::new(value),
        }
    }
}

#[cfg(loom)]
impl<T> SyncUnsafeCell<T> {
    /// `loom`'s `UnsafeCell::new` is not `const`, so the `loom` build
    /// exposes the same constructor without the qualifier.
    pub(crate) fn new(value: T) -> Self {
        Self {
            inner: loom::cell::UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> SyncUnsafeCell<T> {
    /// Run `f` with a `*const T` pointing at the cell contents.
    #[cfg(not(loom))]
    pub(crate) fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(*const T) -> R,
    {
        f(self.inner.get().cast_const())
    }

    /// Run `f` with a `*const T` pointing at the cell contents (`loom`).
    #[cfg(loom)]
    pub(crate) fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(*const T) -> R,
    {
        self.inner.with(f)
    }

    /// Run `f` with a `*mut T` pointing at the cell contents.
    #[cfg(not(loom))]
    pub(crate) fn with_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(*mut T) -> R,
    {
        f(self.inner.get())
    }

    /// Run `f` with a `*mut T` pointing at the cell contents (`loom`).
    #[cfg(loom)]
    pub(crate) fn with_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(*mut T) -> R,
    {
        self.inner.with_mut(f)
    }
}

// SAFETY: `SyncUnsafeCell` exists to be wrapped by primitives that
// themselves provide the synchronisation. Marking it `Sync` therefore
// transfers the safety obligation to the primitive (spinlock, RW lock,
// MCS lock, seqlock, epoch, once), each of which documents how it
// upholds it.
unsafe impl<T: ?Sized + Send> Sync for SyncUnsafeCell<T> {}

/// Declares a `static` holding one of this crate's primitives, or a value that
/// embeds one.
///
/// This is the plain `static` in every build but the model checker's. There a
/// primitive belongs to one model execution, so each execution builds the
/// value afresh on first use, reached through auto-deref as the plain static
/// is. A crate the model checker builds declares every such static this way,
/// since the plain one does not compile there.
///
/// The model's value differs from the plain static in two ways a model must
/// allow for. Its first use synchronises with every later one, which the plain
/// static never does, so a model touches the globals its threads use before
/// spawning them, lest that first use stand in for an ordering the protocol
/// owes. And each execution drops it, which the plain static never is, so a
/// value whose drop reaches another global fails at the execution's end.
///
/// ```
/// use tairix_sync::SpinLock;
///
/// tairix_sync::global! {
///     /// Events seen.
///     static SEEN: SpinLock<u32> = SpinLock::new(0);
/// }
///
/// *SEEN.lock() += 1;
/// assert_eq!(*SEEN.lock(), 1);
/// ```
#[macro_export]
macro_rules! global {
    ($(#[$attr:meta])* static $name:ident: $ty:ty = $init:expr;) => {
        $crate::__global! { [$(#[$attr])*] [] $name: $ty = $init }
    };
    ($(#[$attr:meta])* pub static $name:ident: $ty:ty = $init:expr;) => {
        $crate::__global! { [$(#[$attr])*] [pub] $name: $ty = $init }
    };
    ($(#[$attr:meta])* pub($($scope:tt)+) static $name:ident: $ty:ty = $init:expr;) => {
        $crate::__global! { [$(#[$attr])*] [pub($($scope)+)] $name: $ty = $init }
    };
}

#[cfg(not(loom))]
#[doc(hidden)]
#[macro_export]
macro_rules! __global {
    ([$($attr:tt)*] [$($vis:tt)*] $name:ident: $ty:ty = $init:expr) => {
        $($attr)* $($vis)* static $name: $ty = $init;
    };
}

// Loom's own visibility arms take the visibility as plain tokens, which is
// why `global!` spells each one out rather than taking a `vis` fragment.
#[cfg(loom)]
#[doc(hidden)]
#[macro_export]
macro_rules! __global {
    ([$($attr:tt)*] [$($vis:tt)*] $name:ident: $ty:ty = $init:expr) => {
        $crate::__lazy_static! { $($attr)* $($vis)* static ref $name: $ty = $init; }
    };
}

/// Declares a function that is `const` in every build but the model
/// checker's, whose primitives have no `const` constructor: the constructor
/// of a type embedding one, so the type can back a [`global!`] static.
///
/// ```
/// use tairix_sync::SpinLock;
///
/// pub struct Tally(SpinLock<u32>);
///
/// impl Tally {
///     tairix_sync::const_fn! {
///         /// An empty tally.
///         #[must_use]
///         pub fn new() -> Self {
///             Self(SpinLock::new(0))
///         }
///     }
/// }
///
/// tairix_sync::global! {
///     static TALLY: Tally = Tally::new();
/// }
///
/// *TALLY.0.lock() += 1;
/// assert_eq!(*TALLY.0.lock(), 1);
/// ```
#[macro_export]
macro_rules! const_fn {
    ($(#[$attr:meta])* $vis:vis fn $($signature_and_body:tt)*) => {
        $crate::__const_fn! { [$(#[$attr])*] [$vis] $($signature_and_body)* }
    };
}

#[cfg(not(loom))]
#[doc(hidden)]
#[macro_export]
macro_rules! __const_fn {
    ([$($attr:tt)*] [$vis:vis] $($signature_and_body:tt)*) => {
        $($attr)* $vis const fn $($signature_and_body)*
    };
}

#[cfg(loom)]
#[doc(hidden)]
#[macro_export]
macro_rules! __const_fn {
    ([$($attr:tt)*] [$vis:vis] $($signature_and_body:tt)*) => {
        $($attr)* $vis fn $($signature_and_body)*
    };
}
