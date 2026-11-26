//! `string.h` over guest memory, as RISC OS 5.30's library answers: `memcpy`
//! copying as `memmove` does, `memcmp` and `strncmp` giving the first
//! differing bytes' difference, and `strcmp` its word-at-a-time comparison's
//! answer where both strings are word-aligned.
//!
//! Nothing is read past the page a string's end lies in, so no call reads
//! memory the library's own would not.

use tairix_riscos_arm::mmu::to_page_end;
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::errno::{EDOM, EFBIG, EILSEQ, EOVERFLOW, ERANGE, ESIGNUM, OS_REFUSED};
use super::statics::{at, ERROR_TEXT_BYTES};
use crate::kernel::context;
use crate::kernel::statics::at as kernel_at;
use crate::kernel::statics::Statics;
use crate::module::message;

/// The furthest a string, a number's text or a line may run: the address
/// space.
pub(crate) const ANY_LENGTH: u32 = u32::MAX;
/// The most compared in one read.
const COMPARED: u32 = 256;
/// The message tokens of `strerror`'s texts by `errno`'s value; and the
/// token of a value without one.
const ERRNO_TOKENS: [(u32, &str); 7] = [
    (0, "Errno0"),
    (EDOM, "EDOM"),
    (ERANGE, "ERANGE"),
    (ESIGNUM, "ESIGNUM"),
    (EILSEQ, "EILSEQ"),
    (EOVERFLOW, "EOVERFLOW"),
    (EFBIG, "EFBIG"),
];
const UNKNOWN_ERRNO: &str = "ErrnoUnknown";

/// The length of the string at `s`.
pub(crate) fn length(os: &mut Os<'_>, s: u32) -> Result<u32, Fail> {
    Ok(os
        .position(s, ANY_LENGTH, |b| b == 0)?
        .unwrap_or(ANY_LENGTH))
}

/// Where the string at `s` first holds `c`, its NUL among its bytes.
fn first_of(os: &mut Os<'_>, s: u32, c: u8) -> Result<Option<u32>, Fail> {
    let Some(end) = os
        .as_caller()
        .position(s, ANY_LENGTH, |b| b == c || b == 0)?
    else {
        return Ok(None);
    };
    let at = s.wrapping_add(end);
    Ok((os.as_caller().read_byte(at)? == c).then_some(at))
}

/// The bytes from `address` to its page's end, at most [`COMPARED`].
fn within_page(address: u32) -> u32 {
    to_page_end(address).min(COMPARED)
}

/// Where `first` and `second` first differ within `most` bytes — stopping
/// where both end, if `strings` — and the two bytes there.
fn first_difference(
    os: &mut Os<'_>,
    first: u32,
    second: u32,
    most: u32,
    strings: bool,
) -> Result<Option<(u32, u8, u8)>, Fail> {
    let (mut left, mut right) = ([0; COMPARED as usize], [0; COMPARED as usize]);
    let mut done = 0;
    while done < most {
        let (first_at, second_at) = (first.wrapping_add(done), second.wrapping_add(done));
        let span = within_page(first_at)
            .min(within_page(second_at))
            .min(most - done);
        let (left, right) = (&mut left[..span as usize], &mut right[..span as usize]);
        os.as_caller().read_bytes(first_at, left)?;
        os.as_caller().read_bytes(second_at, right)?;
        for (offset, (&ours, &theirs)) in (0..).zip(left.iter().zip(right.iter())) {
            if ours != theirs {
                return Ok(Some((done + offset, ours, theirs)));
            }
            if strings && ours == 0 {
                return Ok(None);
            }
        }
        done += span;
        os.yield_point()?;
    }
    Ok(None)
}

fn answer(found: Option<(u32, u8, u8)>, shift: impl FnOnce(u32) -> u32) -> u32 {
    found
        .map_or(0, |(at, p, q)| (i32::from(p) - i32::from(q)) << shift(at))
        .cast_unsigned()
}

/// `memmove(s1, s2, n)`, which is `memcpy` and `_memcpy` too.
pub(super) fn memmove(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.as_caller().copy_bytes(regs.r[1], regs.r[0], regs.r[2])
}

/// `memset(s, c, n)`.
pub(super) fn memset(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.as_caller().fill(regs.r[0], regs.r[2], regs.byte(1))
}

/// `_memset(dest, w, n)`: the compiler's fill, `w`'s bytes in turn.
pub(super) fn block_set(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let word = regs.r[1].to_le_bytes();
    let pattern: [u8; COMPARED as usize] = core::array::from_fn(|n| word[n % 4]);
    let (to, bytes) = (regs.r[0], regs.r[2]);
    let mut done = 0;
    while done < bytes {
        let n = (bytes - done).min(COMPARED);
        os.as_caller()
            .write_bytes(to.wrapping_add(done), &pattern[..n as usize])?;
        done += n;
        os.yield_point()?;
    }
    Ok(())
}

/// `strcpy(s1, s2)`.
pub(super) fn strcpy(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let n = length(os, regs.r[1])?;
    os.as_caller()
        .copy_bytes(regs.r[1], regs.r[0], n.saturating_add(1))
}

/// `strncpy(s1, s2, n)`: at most `n` bytes, the rest of `n` noughts.
pub(super) fn strncpy(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (to, from, n) = (regs.r[0], regs.r[1], regs.r[2]);
    let copied = os.as_caller().position(from, n, |b| b == 0)?.unwrap_or(n);
    os.as_caller().copy_bytes(from, to, copied)?;
    os.as_caller().fill(to.wrapping_add(copied), n - copied, 0)
}

/// `strcat(s1, s2)`.
pub(super) fn strcat(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let end = regs.r[0].wrapping_add(length(os, regs.r[0])?);
    let n = length(os, regs.r[1])?;
    os.as_caller()
        .copy_bytes(regs.r[1], end, n.saturating_add(1))
}

/// `strncat(s1, s2, n)`: at most `n` bytes of `s2`, then a nought.
pub(super) fn strncat(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let end = regs.r[0].wrapping_add(length(os, regs.r[0])?);
    let n = os
        .position(regs.r[1], regs.r[2], |b| b == 0)?
        .unwrap_or(regs.r[2]);
    os.as_caller().copy_bytes(regs.r[1], end, n)?;
    os.as_caller().write_byte(end.wrapping_add(n), 0)
}

/// `memcmp(s1, s2, n)`.
pub(super) fn memcmp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let found = first_difference(os, regs.r[0], regs.r[1], regs.r[2], false)?;
    regs.r[0] = answer(found, |_| 0);
    Ok(())
}

/// `strcmp(s1, s2)`: the difference shifted seven bits further for each
/// byte it lies into its word where both strings are word-aligned.
pub(super) fn strcmp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (a, b) = (regs.r[0], regs.r[1]);
    let word_wise = a % 4 == 0 && b % 4 == 0;
    let found = first_difference(os, a, b, ANY_LENGTH, true)?;
    regs.r[0] = answer(found, |at| if word_wise { 7 * (at % 4) } else { 0 });
    Ok(())
}

/// `strncmp(s1, s2, n)`.
pub(super) fn strncmp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let found = first_difference(os, regs.r[0], regs.r[1], regs.r[2], true)?;
    regs.r[0] = answer(found, |_| 0);
    Ok(())
}

/// `memchr(s, c, n)`.
pub(super) fn memchr(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (s, c) = (regs.r[0], regs.byte(1));
    regs.r[0] = os
        .position(s, regs.r[2], |b| b == c)?
        .map_or(0, |n| s.wrapping_add(n));
    Ok(())
}

/// `strchr(s, c)`.
pub(super) fn strchr(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = first_of(os, regs.r[0], regs.byte(1))?.unwrap_or(0);
    Ok(())
}

/// `strrchr(s, c)`.
pub(super) fn strrchr(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (s, c) = (regs.r[0], regs.byte(1));
    let (mut at, mut last) = (0, None);
    os.as_caller().position(s, ANY_LENGTH, |b| {
        if b == c {
            last = Some(at);
        }
        at += 1;
        b == 0
    })?;
    regs.r[0] = last.map_or(0, |n| s.wrapping_add(n));
    Ok(())
}

/// The length of the start of `s1` whose bytes are all among `s2`'s, or
/// where `outside`, none of them.
fn span(os: &mut Os<'_>, s1: u32, s2: u32, outside: bool) -> Result<u32, Fail> {
    let mut set = [false; 256];
    os.as_caller().position(s2, ANY_LENGTH, |b| {
        set[usize::from(b)] = b != 0;
        b == 0
    })?;
    let end = os
        .as_caller()
        .position(s1, ANY_LENGTH, |b| b == 0 || set[usize::from(b)] == outside)?;
    Ok(end.unwrap_or(ANY_LENGTH))
}

/// `strspn(s1, s2)`.
pub(super) fn strspn(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = span(os, regs.r[0], regs.r[1], false)?;
    Ok(())
}

/// `strcspn(s1, s2)`.
pub(super) fn strcspn(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = span(os, regs.r[0], regs.r[1], true)?;
    Ok(())
}

/// `strpbrk(s1, s2)`.
pub(super) fn strpbrk(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = regs.r[0].wrapping_add(span(os, regs.r[0], regs.r[1], true)?);
    regs.r[0] = if os.as_caller().read_byte(at)? == 0 {
        0
    } else {
        at
    };
    Ok(())
}

/// `strstr(s1, s2)`: an empty `s2` is found where `s1` starts.
pub(super) fn strstr(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (haystack, needle) = (regs.r[0], regs.r[1]);
    let wanted = length(os, needle)?;
    if wanted == 0 {
        return Ok(());
    }
    let first = os.as_caller().read_byte(needle)?;
    let mut from = haystack;
    regs.r[0] = loop {
        let Some(at) = first_of(os, from, first)? else {
            break 0;
        };
        if first_difference(os, at, needle, wanted, true)?.is_none() {
            break at;
        }
        from = at.wrapping_add(1);
    };
    Ok(())
}

/// `strtok(s1, s2)`: from `s1`, or where the last call left off where `s1`
/// is nought.
pub(super) fn strtok(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let start = match regs.r[0] {
        0 => statics.read(os, at::TOKEN_NEXT)?,
        given => given,
    };
    if start == 0 {
        return Ok(());
    }
    let delimiters = regs.r[1];
    let token = start.wrapping_add(span(os, start, delimiters, false)?);
    let (next, found) = if os.as_caller().read_byte(token)? == 0 {
        (0, 0)
    } else {
        let end = token.wrapping_add(span(os, token, delimiters, true)?);
        if os.as_caller().read_byte(end)? == 0 {
            (0, token)
        } else {
            os.as_caller().write_byte(end, 0)?;
            (end.wrapping_add(1), token)
        }
    };
    statics.write(os, at::TOKEN_NEXT, next)?;
    regs.r[0] = found;
    Ok(())
}

/// The text for `errno`'s value `n`, written in the library's buffer — or
/// the last OS error's own text where `n` is the OS's refusal: its address.
pub(crate) fn error_text(os: &mut Os<'_>, statics: Statics, n: u32) -> Result<u32, Fail> {
    if n == OS_REFUSED {
        return Ok(statics.field(kernel_at::LAST_ERROR + 4));
    }
    let token = ERRNO_TOKENS
        .iter()
        .find(|&&(value, _)| value == n)
        .map_or(UNKNOWN_ERRNO, |&(_, token)| token);
    let text = message(os, token)?;
    let buffer = statics.field(at::ERROR_TEXT);
    let kept = text.len().min(ERROR_TEXT_BYTES as usize - 1);
    os.as_caller()
        .write_terminated(buffer, &text[..kept], 0, ERROR_TEXT_BYTES as usize)?;
    Ok(buffer)
}

/// `strerror(n)`.
pub(super) fn strerror(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = error_text(os, statics, regs.r[0])?;
    Ok(())
}

/// `strlen(s)`.
pub(super) fn strlen(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = length(os, regs.r[0])?;
    Ok(())
}

#[cfg(test)]
mod tests;
