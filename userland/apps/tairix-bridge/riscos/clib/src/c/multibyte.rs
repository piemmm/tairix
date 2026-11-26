//! The multibyte functions of `stdlib.h`. Every locale RISC OS 5.30 offers
//! has single-byte characters, so a character is its byte, and a wide
//! character above 255 has none.

use tairix_riscos_arm::mmu::to_page_end;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};

/// The widest character a byte holds.
const BYTE_MOST: u32 = 0xFF;
/// The answer for a character there is none of.
const NONE: u32 = u32::MAX;
/// The most converted in one read.
const CHUNK: u32 = 64;

/// `mblen(s, n)`.
pub(super) fn mblen(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (s, n) = (regs.r[0], regs.r[1]);
    regs.r[0] = match (s, n) {
        (0, _) => 0,
        (_, 0) => NONE,
        _ => u32::from(os.as_caller().read_byte(s)? != 0),
    };
    Ok(())
}

/// `mbtowc(pwc, s, n)`.
pub(super) fn mbtowc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (pwc, s, n) = (regs.r[0], regs.r[1], regs.r[2]);
    regs.r[0] = match (s, n) {
        (0, _) => 0,
        (_, 0) => NONE,
        _ => {
            let byte = os.as_caller().read_byte(s)?;
            if pwc != 0 {
                os.as_caller().write_word(pwc, u32::from(byte))?;
            }
            u32::from(byte != 0)
        }
    };
    Ok(())
}

/// `wctomb(s, wc)`.
pub(super) fn wctomb(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (s, wc) = (regs.r[0], regs.r[1]);
    regs.r[0] = match s {
        0 => 0,
        _ if wc > BYTE_MOST => NONE,
        _ => {
            os.as_caller().write_byte(s, wc.to_le_bytes()[0])?;
            1
        }
    };
    Ok(())
}

/// `mbstowcs(pwcs, s, n)`: the characters converted before the NUL, at
/// most `n`, the NUL converted too where it is reached within `n`.
pub(super) fn mbstowcs(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (to, from, most) = (regs.r[0], regs.r[1], regs.r[2]);
    let mut bytes = [0; CHUNK as usize];
    let mut words = [0; CHUNK as usize];
    let mut done = 0;
    while done < most {
        let at = from.wrapping_add(done);
        let n = (most - done).min(CHUNK).min(to_page_end(at));
        let ended = read_up_to_nul(os, at, &mut bytes[..n as usize])?;
        let taken = ended.map_or(n, |end| end + 1) as usize;
        for (word, &byte) in words.iter_mut().zip(&bytes[..taken]) {
            *word = u32::from(byte);
        }
        os.as_caller()
            .write_words(to.wrapping_add(4 * done), &words[..taken])?;
        if let Some(end) = ended {
            regs.r[0] = done + end;
            return Ok(());
        }
        done += n;
    }
    regs.r[0] = most;
    Ok(())
}

/// `out` filled from `at` byte by byte as far as a NUL, the rest left
/// nought, so nothing past a string's end is read: where the NUL is, if
/// it was reached.
fn read_up_to_nul(os: &mut Os<'_>, at: u32, out: &mut [u8]) -> Result<Option<u32>, Fail> {
    let wanted = u32::try_from(out.len()).unwrap_or(u32::MAX);
    let ended = os.as_caller().position(at, wanted, |b| b == 0)?;
    out.fill(0);
    let length = ended.map_or(wanted, |end| end + 1);
    os.as_caller().read_bytes(at, &mut out[..length as usize])?;
    Ok(ended)
}

/// `wcstombs(s, pwcs, n)`: the wide characters converted before the NUL, at
/// most `n` bytes, the NUL converted too where it is reached within `n`;
/// minus one at a wide character no byte holds.
pub(super) fn wcstombs(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (to, from, most) = (regs.r[0], regs.r[1], regs.r[2]);
    let mut words = [0; CHUNK as usize];
    let mut bytes = [0; CHUNK as usize];
    let mut done = 0;
    while done < most {
        let at = from.wrapping_add(4 * done);
        // Whole words within the page, a word that straddles it on its own.
        let n = (most - done).min(CHUNK).min((to_page_end(at) / 4).max(1));
        let words = &mut words[..n as usize];
        os.as_caller().read_words(at, words)?;
        let stop = words.iter().position(|&wc| wc == 0 || wc > BYTE_MOST);
        let ended = stop.filter(|&i| words[i] == 0);
        let kept = stop.map_or(words.len(), |i| i + usize::from(ended.is_some()));
        for (byte, &wc) in bytes.iter_mut().zip(&words[..kept]) {
            *byte = wc.to_le_bytes()[0];
        }
        os.as_caller()
            .write_bytes(to.wrapping_add(done), &bytes[..kept])?;
        if let Some(i) = stop {
            regs.r[0] = ended.map_or(NONE, |_| done + len32(i));
            return Ok(());
        }
        done += n;
    }
    regs.r[0] = most;
    Ok(())
}

#[cfg(test)]
mod tests;
