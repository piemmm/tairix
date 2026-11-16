//! Commodore program files, loaded straight into memory: a PRG is its load
//! address, low byte first, then its bytes; a T64 is an emulators' tape
//! archive of such files, each with its start address and its bytes' offset
//! in the archive, of which LOAD finds the first.
//!
//! A T64 entry's end address is often wrong — archivers wrote one tape's
//! length for every file — so an entry runs no further than the next
//! entry's bytes, or the archive's end.

use alloc::vec::Vec;

/// A program and where it loads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// The address its first byte loads at.
    pub address: u16,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// Why a file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramError {
    /// Too short to hold a load address, or a T64 with another signature.
    BadHeader,
    /// Bytes that would load past the top of memory, or an entry pointing
    /// outside the archive.
    Malformed,
    /// An archive holding no program.
    Empty,
}

/// The top of memory a program may reach.
const MEMORY: usize = 0x1_0000;

/// A PRG file's program.
///
/// # Errors
///
/// [`ProgramError::BadHeader`] for a file shorter than its load address,
/// and [`ProgramError::Malformed`] for one loading past `$FFFF`.
pub fn decode_prg(file: &[u8]) -> Result<Program, ProgramError> {
    let (address, bytes) = file.split_at_checked(2).ok_or(ProgramError::BadHeader)?;
    let address = u16::from_le_bytes([address[0], address[1]]);
    if usize::from(address) + bytes.len() > MEMORY {
        return Err(ProgramError::Malformed);
    }
    Ok(Program {
        address,
        bytes: bytes.to_vec(),
    })
}

/// The signatures T64 archives open with.
const T64_SIGNATURES: [&[u8]; 2] = [b"C64 tape image file", b"C64S tape"];
/// The header, and each directory entry.
const T64_HEADER: usize = 0x40;
const T64_ENTRY: usize = 0x20;
/// An entry holding a program.
const T64_FILE: u8 = 1;
/// The most entries looked at.
const T64_MAX_ENTRIES: usize = 0x1000;

fn le16(bytes: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(u16::from_le_bytes([
        *bytes.get(at)?,
        *bytes.get(at + 1)?,
    ])))
}

fn le32(bytes: &[u8], at: usize) -> Option<usize> {
    let field = bytes.get(at..at + 4)?;
    usize::try_from(u32::from_le_bytes([field[0], field[1], field[2], field[3]])).ok()
}

/// A T64 archive's first program, running no further than the bytes of the
/// entry after it in the archive.
///
/// # Errors
///
/// [`ProgramError::BadHeader`] for another signature,
/// [`ProgramError::Malformed`] for an entry outside the archive or a
/// program loading past `$FFFF`, and [`ProgramError::Empty`] for an archive
/// with no program.
pub fn decode_t64(file: &[u8]) -> Result<Program, ProgramError> {
    if !T64_SIGNATURES
        .iter()
        .any(|signature| file.starts_with(signature))
    {
        return Err(ProgramError::BadHeader);
    }
    let slots = le16(file, 0x22)
        .ok_or(ProgramError::BadHeader)?
        .min(T64_MAX_ENTRIES);
    let mut first = None;
    let mut offsets = Vec::new();
    for slot in 0..slots {
        let at = T64_HEADER + slot * T64_ENTRY;
        let Some(entry) = file.get(at..at + T64_ENTRY) else {
            break;
        };
        if entry[0] != T64_FILE {
            continue;
        }
        let start = le16(entry, 2).ok_or(ProgramError::Malformed)?;
        let end = le16(entry, 4).ok_or(ProgramError::Malformed)?;
        let offset = le32(entry, 8).ok_or(ProgramError::Malformed)?;
        // Every offset is held to the archive before it bounds another entry.
        if offset > file.len() {
            return Err(ProgramError::Malformed);
        }
        offsets.push(offset);
        first.get_or_insert((start, end, offset));
    }
    let (start, end, offset) = first.ok_or(ProgramError::Empty)?;
    offsets.sort_unstable();
    let next = offsets
        .get(offsets.partition_point(|&other| other <= offset))
        .copied()
        .unwrap_or(file.len());
    let stated = end.saturating_sub(start);
    let len = if stated == 0 {
        next - offset
    } else {
        stated.min(next - offset)
    };
    if start + len > MEMORY {
        return Err(ProgramError::Malformed);
    }
    Ok(Program {
        address: u16::try_from(start).map_err(|_| ProgramError::Malformed)?,
        bytes: file[offset..offset + len].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prg_is_its_load_address_then_its_bytes() {
        let program = decode_prg(&[0x01, 0x08, 0xAA, 0xBB]).expect("a PRG");
        assert_eq!(
            (program.address, program.bytes.as_slice()),
            (0x0801, &[0xAA, 0xBB][..])
        );
        assert_eq!(decode_prg(&[0x01]), Err(ProgramError::BadHeader));
        assert_eq!(
            decode_prg(&[0xFF, 0xFF, 1, 2]),
            Err(ProgramError::Malformed)
        );
    }

    /// A T64 of `files`, each (start, stated end, bytes, name).
    fn t64(files: &[(u16, u16, &[u8], &str)]) -> Vec<u8> {
        let mut file = Vec::from(&b"C64S tape image file"[..]);
        file.resize(0x20, 0);
        file.extend([0x01, 0x01]);
        let count = u16::try_from(files.len()).expect("few");
        file.extend(count.to_le_bytes());
        file.extend(count.to_le_bytes());
        file.resize(T64_HEADER, 0x20);
        let mut offset = T64_HEADER + files.len() * T64_ENTRY;
        for &(start, end, bytes, name) in files {
            let mut entry = alloc::vec![T64_FILE, 0x82];
            entry.extend(start.to_le_bytes());
            entry.extend(end.to_le_bytes());
            entry.extend([0, 0]);
            entry.extend(u32::try_from(offset).expect("small").to_le_bytes());
            entry.extend([0; 4]);
            let mut name = Vec::from(name.as_bytes());
            name.resize(16, b' ');
            entry.extend(name);
            file.extend(entry);
            offset += bytes.len();
        }
        for &(_, _, bytes, _) in files {
            file.extend_from_slice(bytes);
        }
        file
    }

    #[test]
    fn a_t64_s_first_program_runs_no_further_than_the_next() {
        let archive = t64(&[
            (0x0801, 0xC3C6, &[1, 2, 3], "FIRST"),
            (0xC000, 0xC002, &[4, 5], "SECOND"),
        ]);
        assert_eq!(
            decode_t64(&archive),
            Ok(Program {
                address: 0x0801,
                bytes: alloc::vec![1, 2, 3],
            }),
            "its stated end was every archiver's favourite"
        );
    }

    /// The regression: every entry's bytes were copied though only the
    /// first is loaded, so entries sharing one offset made one small file
    /// copy a program's bytes thousands of times over.
    #[test]
    fn only_the_first_program_is_taken_however_many_entries_share_its_bytes() {
        let bytes = alloc::vec![0xEA; 0x4000];
        let files: Vec<(u16, u16, &[u8], &str)> = (0..T64_MAX_ENTRIES)
            .map(|_| (0x0801u16, 0u16, &bytes[..], "SAME"))
            .collect();
        let mut archive = t64(&files);
        let shared = T64_HEADER + T64_MAX_ENTRIES * T64_ENTRY;
        let shared = u32::try_from(shared).expect("small").to_le_bytes();
        for slot in 0..T64_MAX_ENTRIES {
            let at = T64_HEADER + slot * T64_ENTRY + 8;
            archive[at..at + 4].copy_from_slice(&shared);
        }
        archive.truncate(T64_HEADER + T64_MAX_ENTRIES * T64_ENTRY + bytes.len());
        let program = decode_t64(&archive).expect("a T64");
        assert_eq!(
            (program.address, program.bytes.len()),
            (0x0801, bytes.len())
        );
    }

    #[test]
    fn malformed_archives_are_refused() {
        assert_eq!(decode_t64(b"not a tape"), Err(ProgramError::BadHeader));
        assert_eq!(decode_t64(&t64(&[])), Err(ProgramError::Empty));
        let mut archive = t64(&[(0x0801, 0x0804, &[1, 2, 3], "X")]);
        archive[T64_HEADER + 8..T64_HEADER + 12].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
        assert_eq!(decode_t64(&archive), Err(ProgramError::Malformed));
    }

    /// The regression: a later entry past the archive's end bounded an
    /// earlier one's bytes before it was refused, and the slice panicked.
    #[test]
    fn an_entry_past_the_end_is_refused_before_it_bounds_another() {
        let mut archive = t64(&[(0x0801, 0, &[1, 2, 3], "X"), (0xC000, 0, &[4], "Y")]);
        let second = T64_HEADER + T64_ENTRY + 8;
        let past = u32::try_from(archive.len() + 10).expect("small");
        archive[second..second + 4].copy_from_slice(&past.to_le_bytes());
        assert_eq!(decode_t64(&archive), Err(ProgramError::Malformed));
    }
}
