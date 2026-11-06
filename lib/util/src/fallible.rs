//! Fallible allocation for buffers whose size comes from the data.
//!
//! Userland's heap answers exhaustion with a null pointer, which `alloc`
//! turns into `handle_alloc_error` — a process abort. A buffer sized by
//! image geometry or by a decoded payload is close to a megabyte at desktop
//! scale, so it is what a machine short of memory actually refuses, and a
//! program that has to degrade through that state cannot reach it through an
//! infallible growth. These reserve first and report the refusal, so it
//! reaches the caller that already publishes one.
//!
//! Two growth policies, because the buffers have two lifetimes: a one-shot
//! buffer reserves exactly what it needs, and a scratch that is grown across
//! uses reserves amortised so repeated growth is not quadratic.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::fmt;

use tairix_abi::Errno;

/// `count` copies of `value`, or `None` when the allocator refuses room for
/// them.
pub fn filled<T: Clone>(count: usize, value: T) -> Option<Vec<T>> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(count).ok()?;
    buffer.resize(count, value);
    Some(buffer)
}

/// `count` zero bytes, or `None` when the allocator refuses room for them.
///
/// Asked of the allocator as zeroed memory, so one handing out pages still as
/// the kernel mapped them writes none of them: a large buffer its writer goes
/// on to fill only in part costs the pages written, not its length.
#[must_use]
pub fn zeroed(count: usize) -> Option<Vec<u8>> {
    if count == 0 {
        return Some(Vec::new());
    }
    let layout = Layout::array::<u8>(count).ok()?;
    // SAFETY: `layout` is not zero-sized.
    let block = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if block.is_null() {
        return None;
    }
    // SAFETY: the global allocator gave `block` for `layout`, `count` bytes at
    // the alignment of `u8`, the allocation a boxed slice of `count` bytes
    // holds and frees with; it zeroed all of them, and a zero is a `u8`.
    let bytes = unsafe { Box::from_raw(core::ptr::slice_from_raw_parts_mut(block, count)) };
    Some(bytes.into_vec())
}

/// `N` copies of `value` in a boxed array, or `None` when the allocator
/// refuses room for them. Built on the heap, never staged on the stack, so a
/// sector-sized buffer costs a caller on a small kernel stack nothing.
pub fn boxed_array<T: Clone, const N: usize>(value: T) -> Option<Box<[T; N]>> {
    filled(N, value)?.into_boxed_slice().try_into().ok()
}

/// The first `count` of `items`, or `None` when the allocator refuses room
/// for them.
///
/// The take is what keeps the reservation exact: an iterator yielding more
/// than it claimed would otherwise grow the buffer past what was reserved.
pub fn collected<T>(count: usize, items: impl Iterator<Item = T>) -> Option<Vec<T>> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(count).ok()?;
    buffer.extend(items.take(count));
    Some(buffer)
}

/// A copy of `items`, or `None` when the allocator refuses room for it.
#[must_use]
pub fn copied<T: Copy>(items: &[T]) -> Option<Vec<T>> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(items.len()).ok()?;
    copy.extend_from_slice(items);
    Some(copy)
}

/// Lengthen a reused `scratch` to `count` copies, padding with `value`,
/// reporting whether it now holds them. A scratch already that long keeps
/// its capacity.
#[must_use]
pub fn grow_to<T: Clone>(scratch: &mut Vec<T>, count: usize, value: T) -> bool {
    let Some(extra) = count.checked_sub(scratch.len()) else {
        return true;
    };
    if scratch.try_reserve(extra).is_err() {
        return false;
    }
    scratch.resize(count, value);
    true
}

/// Read a whole snapshot through `read` into `buffer`, answering its length:
/// for a source that copies all of it or none and does not say how much it
/// needs, such as the hardware tree. An empty `buffer` starts at `initial`
/// bytes, and each [`Errno::BufferTooSmall`] doubles it, so a snapshot of any
/// size is read rather than refused.
///
/// # Errors
///
/// Any other refusal `read` answers, [`Errno::OutOfMemory`] when the buffer
/// cannot grow, or [`Errno::OutOfRange`] when doubling it would overflow.
pub fn read_snapshot(
    buffer: &mut Vec<u8>,
    initial: usize,
    mut read: impl FnMut(&mut [u8]) -> Result<usize, Errno>,
) -> Result<usize, Errno> {
    if buffer.is_empty() && !grow_to(buffer, initial.max(1), 0) {
        return Err(Errno::OutOfMemory);
    }
    loop {
        match read(buffer.as_mut_slice()) {
            Err(Errno::BufferTooSmall) => {
                let doubled = buffer.len().checked_mul(2).ok_or(Errno::OutOfRange)?;
                if !grow_to(buffer, doubled, 0) {
                    return Err(Errno::OutOfMemory);
                }
            }
            answered => return answered,
        }
    }
}

/// `parts` joined end to end in a string of exactly their length, or `None`
/// when the allocator refuses it.
#[must_use]
pub fn concatenated(parts: &[&str]) -> Option<String> {
    let len = parts
        .iter()
        .try_fold(0usize, |len, part| len.checked_add(part.len()))?;
    let mut joined = String::new();
    joined.try_reserve_exact(len).ok()?;
    for part in parts {
        joined.push_str(part);
    }
    Some(joined)
}

/// What `write` writes, in a string reserved once to its exact length, or
/// `None` when the allocator refuses it or `write` fails: `write` runs
/// twice, measuring and then filling, and a fill writing past what was
/// measured fails rather than growing the string.
#[must_use]
pub fn written(write: impl Fn(&mut dyn fmt::Write) -> fmt::Result) -> Option<String> {
    struct Measured(usize);
    impl fmt::Write for Measured {
        fn write_str(&mut self, part: &str) -> fmt::Result {
            self.0 = self.0.checked_add(part.len()).ok_or(fmt::Error)?;
            Ok(())
        }
    }
    struct Within<'s>(&'s mut String);
    impl fmt::Write for Within<'_> {
        fn write_str(&mut self, part: &str) -> fmt::Result {
            if self.0.capacity() - self.0.len() < part.len() {
                return Err(fmt::Error);
            }
            self.0.push_str(part);
            Ok(())
        }
    }
    let mut measured = Measured(0);
    write(&mut measured).ok()?;
    let mut out = String::new();
    out.try_reserve_exact(measured.0).ok()?;
    write(&mut Within(&mut out)).ok()?;
    Some(out)
}

/// Room in `buffer` for `count` more items, reporting whether the allocator
/// granted it — for a buffer filled by pushing rather than by resizing.
#[must_use]
pub fn reserve<T>(buffer: &mut Vec<T>, count: usize) -> bool {
    buffer.try_reserve_exact(count).is_ok()
}

/// Append `item` to `buffer`, reporting whether the allocator granted the
/// room: a full buffer's room doubles, so a list pushed an item at a time
/// costs amortised constant time where reserving each item exactly would
/// copy the whole list on every push.
#[must_use]
pub fn push<T>(buffer: &mut Vec<T>, item: T) -> bool {
    if buffer.len() == buffer.capacity() && buffer.try_reserve(1).is_err() {
        return false;
    }
    buffer.push(item);
    true
}

/// Move `buffer` into an allocation of exactly its length where the allocator
/// grants one, and leave it as it is where none is: a refusal never aborts, as
/// `Vec::shrink_to_fit`'s does.
pub fn trim<T>(buffer: &mut Vec<T>) {
    if buffer.capacity() == buffer.len() {
        return;
    }
    let mut exact = Vec::new();
    if exact.try_reserve_exact(buffer.len()).is_ok() {
        exact.append(buffer);
        *buffer = exact;
    }
}

/// The bytes `buffer`'s allocation holds: its capacity's worth, not its
/// length's, as that is what the allocator gave it.
#[must_use]
pub fn held<T>(buffer: &Vec<T>) -> u64 {
    (buffer.capacity() as u64).saturating_mul(core::mem::size_of::<T>() as u64)
}

#[cfg(test)]
#[path = "fallible_tests.rs"]
mod tests;
