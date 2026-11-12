//! Work in progress kept in guest memory: a stack of frames, each one
//! routine's phase and locals, stepped in Rust until a routine needs guest
//! code and resumed by the host call that code returns into. TAIRiX DOS's
//! kernel keeps its calls this way in the swappable data area; a program
//! whose behaviour is Rust keeps its own in its own memory.
//!
//! A *root* is work begun from outside — a call made to the kernel, a
//! program started, a handler entered — and the frames above it are the
//! routines it has called. Every frame is decoded from guest memory and
//! checked, so guest code that writes garbage over the frames fails the
//! work; it cannot make the host misbehave.

/// Bytes of a routine's locals.
pub const LOCALS: usize = 40;
/// Bytes of a frame: its routine, phase and child's outcome, then locals.
pub const FRAME: u16 = 48;
const _: () = assert!(FRAME as usize == 8 + LOCALS);

/// The extended error a routine gets back when the stack cannot hold or
/// decode its work: DOS's general failure.
pub const GENERAL_FAILURE: u16 = 0x1F;

/// Why a routine did not finish its work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// It failed with this extended error.
    Error(u16),
    /// An INT 24h handler chose to abort: the program ends.
    Abort,
    /// Ctrl+C or Ctrl+Break: the work is abandoned for INT 23h.
    Break,
}

/// What a routine finished with.
pub type Outcome = Result<u32, Fault>;

/// What a routine's step asks for next.
#[derive(Clone, Copy, Debug)]
pub enum Flow<I> {
    /// It is done.
    Return(Outcome),
    /// Run this routine first; this one continues with its outcome.
    Call(Frame<I>),
    /// It has set guest code going, which returns to the host.
    Wait,
    /// The root's work is over, and the CPU is already set going elsewhere.
    Transfer,
}

/// A frame, decoded.
#[derive(Clone, Copy, Debug)]
pub struct Frame<I> {
    /// The routine it runs.
    pub id: I,
    /// Where the routine has reached.
    pub phase: u8,
    /// The outcome of the routine it last called.
    pub child: Outcome,
    /// Its locals.
    pub locals: [u8; LOCALS],
}

impl<I> Frame<I> {
    /// A frame starting routine `id` with `locals`.
    pub fn new(id: I, locals: &impl Locals) -> Self {
        let mut bytes = [0; LOCALS];
        locals.store(&mut bytes);
        Self {
            id,
            phase: 0,
            child: Ok(0),
            locals: bytes,
        }
    }
}

/// A table of routines, as their frames number them.
pub trait Identity: Copy + core::fmt::Debug {
    /// The number a frame of it records; none is zero, so bytes no routine
    /// wrote are no frame.
    fn number(self) -> u8;
    /// The routine numbered `byte`, if one is.
    fn of(byte: u8) -> Option<Self>;
    /// Whether frames of it are roots.
    fn is_root(self) -> bool;
}

/// A routine's locals, packed into its frame.
pub trait Locals: Sized {
    /// The locals `bytes` hold.
    fn load(bytes: &[u8; LOCALS]) -> Self;
    /// Packs them into `bytes`.
    fn store(&self, bytes: &mut [u8; LOCALS]);
}

/// A field a routine keeps in its locals.
pub trait Field: Copy {
    /// Bytes it takes.
    const SIZE: usize;
    /// Reads it at `*at`, moving past it.
    fn take(bytes: &[u8], at: &mut usize) -> Self;
    /// Writes it at `*at`, moving past it.
    fn put(self, bytes: &mut [u8], at: &mut usize);
}

impl<const N: usize> Field for [u8; N] {
    const SIZE: usize = N;
    fn take(bytes: &[u8], at: &mut usize) -> Self {
        let mut out = [0; N];
        if let Some(from) = bytes.get(*at..*at + N) {
            out.copy_from_slice(from);
        }
        *at += N;
        out
    }
    fn put(self, bytes: &mut [u8], at: &mut usize) {
        if let Some(to) = bytes.get_mut(*at..*at + N) {
            to.copy_from_slice(&self);
        }
        *at += N;
    }
}

impl<const N: usize> Field for [u16; N] {
    const SIZE: usize = 2 * N;
    fn take(bytes: &[u8], at: &mut usize) -> Self {
        core::array::from_fn(|_| u16::take(bytes, at))
    }
    fn put(self, bytes: &mut [u8], at: &mut usize) {
        for word in self {
            word.put(bytes, at);
        }
    }
}

macro_rules! integer_field {
    ($($ty:ty),*) => {$(
        impl Field for $ty {
            const SIZE: usize = core::mem::size_of::<$ty>();
            fn take(bytes: &[u8], at: &mut usize) -> Self {
                Self::from_le_bytes(<[u8; core::mem::size_of::<$ty>()]>::take(bytes, at))
            }
            fn put(self, bytes: &mut [u8], at: &mut usize) {
                self.to_le_bytes().put(bytes, at);
            }
        }
    )*};
}
integer_field!(u8, u16, u32, i16);

impl Field for bool {
    const SIZE: usize = 1;
    fn take(bytes: &[u8], at: &mut usize) -> Self {
        u8::take(bytes, at) != 0
    }
    fn put(self, bytes: &mut [u8], at: &mut usize) {
        u8::from(self).put(bytes, at);
    }
}

/// A value that may be absent: a byte saying whether it is there, then its
/// own bytes, written as its default where it is not.
impl<T: Field + Default> Field for Option<T> {
    const SIZE: usize = 1 + T::SIZE;
    fn take(bytes: &[u8], at: &mut usize) -> Self {
        let there = bool::take(bytes, at);
        let value = T::take(bytes, at);
        there.then_some(value)
    }
    fn put(self, bytes: &mut [u8], at: &mut usize) {
        self.is_some().put(bytes, at);
        self.unwrap_or_default().put(bytes, at);
    }
}

/// Declares a struct of [`Field`]s that is itself a field, packed in
/// order: state larger than a routine's locals, kept in a program's own
/// memory.
macro_rules! record {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($(#[$fmeta:meta])* $field:ident: $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        $vis struct $name { $($(#[$fmeta])* pub $field: $ty),* }

        impl $crate::frames::Field for $name {
            const SIZE: usize = 0 $(+ <$ty as $crate::frames::Field>::SIZE)*;
            fn take(bytes: &[u8], at: &mut usize) -> Self {
                Self { $($field: $crate::frames::Field::take(bytes, at)),* }
            }
            fn put(self, bytes: &mut [u8], at: &mut usize) {
                $($crate::frames::Field::put(self.$field, bytes, at);)*
            }
        }
    };
}
pub(crate) use record;

/// The low and high words of `value`.
#[must_use]
pub fn words(value: u32) -> (u16, u16) {
    let [low0, low1, high0, high1] = value.to_le_bytes();
    (
        u16::from_le_bytes([low0, low1]),
        u16::from_le_bytes([high0, high1]),
    )
}

/// A far pointer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Far {
    /// Its segment.
    pub segment: u16,
    /// Its offset.
    pub offset: u16,
}

impl Far {
    /// `segment:offset`.
    #[must_use]
    pub const fn new(segment: u16, offset: u16) -> Self {
        Self { segment, offset }
    }

    /// The pointer `n` bytes on, its offset wrapping in its segment.
    #[must_use]
    pub const fn add(self, n: u16) -> Self {
        Self::new(self.segment, self.offset.wrapping_add(n))
    }

    /// Its linear address.
    #[must_use]
    pub fn linear(self) -> u32 {
        crate::cpu::linear(self.segment, self.offset)
    }
}

impl Field for Far {
    const SIZE: usize = 4;
    fn take(bytes: &[u8], at: &mut usize) -> Self {
        let offset = u16::take(bytes, at);
        Self::new(u16::take(bytes, at), offset)
    }
    fn put(self, bytes: &mut [u8], at: &mut usize) {
        self.offset.put(bytes, at);
        self.segment.put(bytes, at);
    }
}

/// Declares a routine's locals: a struct of [`Field`]s packed in order.
macro_rules! locals {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($(#[$fmeta:meta])* $field:ident: $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        $vis struct $name { $($(#[$fmeta])* pub $field: $ty),* }

        impl $crate::frames::Locals for $name {
            #[allow(unused_variables, unused_mut, reason = "a routine may keep no locals")]
            fn load(bytes: &[u8; $crate::frames::LOCALS]) -> Self {
                let mut at = 0;
                Self { $($field: $crate::frames::Field::take(bytes, &mut at)),* }
            }
            #[allow(unused_variables, unused_mut, reason = "a routine may keep no locals")]
            fn store(&self, bytes: &mut [u8; $crate::frames::LOCALS]) {
                let mut at = 0;
                $($crate::frames::Field::put(self.$field, bytes, &mut at);)*
            }
        }

        const _: () = assert!(
            $crate::frames::LOCALS.checked_sub(0 $(+ <$ty as $crate::frames::Field>::SIZE)*).is_some()
        );
    };
}
pub(crate) use locals;

/// The value of a child's outcome, or the flow that hands its fault on.
macro_rules! take {
    ($child:expr) => {
        match $child {
            Ok(value) => value,
            Err(fault) => return $crate::frames::Flow::Return(Err(fault)),
        }
    };
}
pub(crate) use take;

/// Declares a table of routines run over `$context`, each implementing
/// `$trait`: the [`Identity`] its frames number them by, and the step that
/// runs one. A routine's number is its place in the list, plus one.
macro_rules! routines {
    ($context:ty, $trait:path; $($id:ident => $routine:ty),* $(,)?) => {
        /// The routines, by the number their frames record.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u8)]
        #[allow(missing_docs, reason = "each is the routine of its name")]
        pub enum Id {
            $($id,)*
        }

        impl Id {
            const ALL: &'static [Self] = &[$(Self::$id),*];

            /// The number a frame of it records; none is zero, so bytes no
            /// routine wrote are no frame.
            #[must_use]
            pub const fn number(self) -> u8 {
                self as u8 + 1
            }

            /// The routine numbered `byte`, if one is.
            #[must_use]
            pub fn of(byte: u8) -> Option<Self> {
                Self::ALL.get(usize::from(byte.checked_sub(1)?)).copied()
            }
        }

        /// Runs `frame`'s routine on from its phase: its locals decoded,
        /// stepped and stored back.
        pub fn step(c: &mut $context, frame: &mut $crate::frames::Frame<Id>) -> $crate::frames::Flow<Id> {
            fn run<R: $trait>(c: &mut $context, frame: &mut $crate::frames::Frame<Id>) -> $crate::frames::Flow<Id> {
                let mut routine = <R as $crate::frames::Locals>::load(&frame.locals);
                let child = frame.child;
                let flow = routine.step(c, &mut frame.phase, child);
                $crate::frames::Locals::store(&routine, &mut frame.locals);
                flow
            }
            match frame.id {
                $(Id::$id => run::<$routine>(c, frame),)*
            }
        }
    };
}
pub(crate) use routines;

const NONE: u8 = 0;
const ERROR: u8 = 1;
const ABORT: u8 = 2;
const BREAK: u8 = 3;

/// `frame` as its bytes in guest memory.
pub fn encode<I: Identity>(frame: &Frame<I>) -> [u8; FRAME as usize] {
    let mut raw = [0u8; FRAME as usize];
    raw[0] = frame.id.number();
    raw[1] = frame.phase;
    let (tag, value) = match frame.child {
        Ok(value) => (NONE, value),
        Err(Fault::Error(code)) => (ERROR, u32::from(code)),
        Err(Fault::Abort) => (ABORT, 0),
        Err(Fault::Break) => (BREAK, 0),
    };
    raw[2] = tag;
    raw[4..8].copy_from_slice(&value.to_le_bytes());
    raw[8..].copy_from_slice(&frame.locals);
    raw
}

/// The frame `raw` holds, or `None` for bytes no routine wrote.
pub fn decode<I: Identity>(raw: &[u8; FRAME as usize]) -> Option<Frame<I>> {
    let value = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
    let child = match raw[2] {
        NONE => Ok(value),
        ERROR => Err(Fault::Error(u16::try_from(value).ok()?)),
        ABORT => Err(Fault::Abort),
        BREAK => Err(Fault::Break),
        _ => return None,
    };
    let mut locals = [0; LOCALS];
    locals.copy_from_slice(&raw[8..]);
    Some(Frame {
        id: I::of(raw[0])?,
        phase: raw[1],
        child,
        locals,
    })
}

/// Where a stack of frames lies and what its roots' ends do.
pub trait Context {
    /// The routines its frames run.
    type Id: Identity;
    /// Frames it holds.
    const DEPTH: u8;
    /// Frames in use.
    fn depth(&mut self) -> u8;
    fn set_depth(&mut self, depth: u8);
    /// The frame the running root is.
    fn root_index(&mut self) -> u8;
    /// Frame `n`, decoded, or `None` for bytes no routine wrote.
    fn load(&mut self, n: u8) -> Option<Frame<Self::Id>>;
    fn store(&mut self, n: u8, frame: &Frame<Self::Id>);
    /// Steps `frame`'s routine on from its phase.
    fn step(&mut self, frame: &mut Frame<Self::Id>) -> Flow<Self::Id>;
    /// The running root returned `outcome`: its frames are dropped and its
    /// caller continues.
    fn returned(&mut self, outcome: Outcome);
    /// The running root's work is over and the CPU already set going
    /// elsewhere: its frames are dropped.
    fn transferred(&mut self);
    /// The running root's frames no longer decode: its work fails.
    fn broken(&mut self);
}

/// Steps the top frame of `c` until a routine waits or a root ends.
pub fn run<C: Context>(c: &mut C) {
    loop {
        let depth = c.depth().min(C::DEPTH);
        let Some(top) = depth.checked_sub(1) else {
            return;
        };
        let Some(mut frame) = c.load(top) else {
            return c.broken();
        };
        let root = c.root_index() == top;
        let flow = if frame.id.is_root() == root {
            c.step(&mut frame)
        } else {
            Flow::Return(Err(Fault::Error(GENERAL_FAILURE)))
        };
        match flow {
            Flow::Wait => {
                frame.child = Ok(0);
                c.store(top, &frame);
                return;
            }
            Flow::Transfer => return c.transferred(),
            Flow::Call(child) => {
                if depth >= C::DEPTH {
                    frame.child = Err(Fault::Error(GENERAL_FAILURE));
                    c.store(top, &frame);
                } else {
                    c.store(top, &frame);
                    c.store(depth, &child);
                    c.set_depth(depth + 1);
                }
            }
            Flow::Return(outcome) if root => return c.returned(outcome),
            Flow::Return(outcome) => {
                let Some(parent) = top.checked_sub(1) else {
                    return c.broken();
                };
                c.set_depth(top);
                let Some(mut caller) = c.load(parent) else {
                    return c.broken();
                };
                caller.child = outcome;
                c.store(parent, &caller);
            }
        }
    }
}

#[cfg(test)]
#[path = "frames_tests.rs"]
mod tests;
