//! The library's chunks: each one's entries, as routines of the module's
//! image, and its part of the library's statics. A chunk is listed once
//! every entry it holds is built, so the library answers as a release that
//! had only the chunks listed would.

use alloc::vec::Vec;

use tairix_riscos_arm::instruction::{BlockMode, BlockOp, Op};
use tairix_riscos_arm::reg::{Reg, RegList};
use tairix_riscos_kernel::memory::MemoryError;
use tairix_riscos_kernel::module::{instruction, Code};

/// A chunk's entry: a routine of its own, or the routine another chunk's
/// entry is, which RISC OS 5.30's vectors share.
#[derive(Clone, Copy)]
pub(crate) enum Entry {
    Own(Code),
    Shared { chunk: u32, entry: usize },
}

/// Every entry of `codes` a routine of its own.
pub(crate) const fn own<const N: usize>(codes: [Code; N]) -> [Entry; N] {
    let mut entries = [Entry::Shared { chunk: 0, entry: 0 }; N];
    let mut n = 0;
    while n < N {
        entries[n] = Entry::Own(codes[n]);
        n += 1;
    }
    entries
}

/// A chunk of the library.
#[derive(Clone, Copy)]
pub(crate) struct Chunk {
    /// The id a stub description names it by.
    pub(crate) id: u32,
    entries: &'static [Entry],
    /// Its statics: where they start in the library's, and their size.
    pub(crate) statics_at: u32,
    pub(crate) statics: u32,
}

impl Chunk {
    /// How many entries a stub may ask for.
    pub(crate) const fn entries(&self) -> usize {
        self.entries.len()
    }
}

/// The chunks built, each entry sharing only with one before it: the library
/// kernel, the C library, the C99 library's low-level and high-level
/// parts, the atomic operations, and the parts of the C99 library RISC OS
/// 5.30 also gives chunks of their own.
static CHUNKS: [Chunk; 8] = [
    Chunk {
        id: 1,
        entries: &KERNEL,
        statics_at: 0,
        statics: crate::kernel::statics::SIZE,
    },
    Chunk {
        id: 2,
        entries: &crate::c::ENTRIES,
        statics_at: crate::c::statics::START,
        statics: crate::c::statics::SIZE,
    },
    Chunk {
        id: 4,
        entries: &crate::c99low::ENTRIES,
        statics_at: 0,
        statics: 0,
    },
    Chunk {
        id: 5,
        entries: &crate::c::c99::ENTRIES,
        statics_at: 0,
        statics: 0,
    },
    Chunk {
        id: 6,
        entries: &crate::atomic::ENTRIES,
        statics_at: 0,
        statics: 0,
    },
    Chunk {
        id: 7,
        entries: &crate::c::c99::MATHS,
        statics_at: 0,
        statics: 0,
    },
    Chunk {
        id: 8,
        entries: &crate::c::c99::COMPLEX,
        statics_at: 0,
        statics: 0,
    },
    Chunk {
        id: 9,
        entries: &crate::c::c99::EXTRAS,
        statics_at: 0,
        statics: 0,
    },
];

const KERNEL: [Entry; 48] = own(crate::kernel::ENTRIES);

/// The chunk `id`; none for a chunk the library does not hold.
pub(crate) fn chunk(id: u32) -> Option<&'static Chunk> {
    CHUNKS.iter().find(|chunk| chunk.id == id)
}

/// Where in [`CHUNKS`] the chunk `id` is.
const fn position(id: u32) -> usize {
    let mut n = 0;
    while CHUNKS[n].id != id {
        n += 1;
    }
    n
}

/// Every chunk's entries, chunk by chunk.
const ENTRY_COUNT: usize = {
    let mut count = 0;
    let mut n = 0;
    while n < CHUNKS.len() {
        count += CHUNKS[n].entries.len();
        n += 1;
    }
    count
};

/// Where each chunk's entries start among every chunk's.
const FIRSTS: [usize; CHUNKS.len()] = {
    let mut firsts = [0; CHUNKS.len()];
    let mut n = 1;
    while n < CHUNKS.len() {
        firsts[n] = firsts[n - 1] + CHUNKS[n - 1].entries.len();
        n += 1;
    }
    firsts
};

/// The routines the chunks' own entries are.
const OWN_ENTRIES: usize = {
    let mut count = 0;
    let mut n = 0;
    while n < CHUNKS.len() {
        let mut e = 0;
        while e < CHUNKS[n].entries.len() {
            if matches!(CHUNKS[n].entries[e], Entry::Own(_)) {
                count += 1;
            }
            e += 1;
        }
        n += 1;
    }
    count
};

/// Where the library's own routines start among [`ROUTINES`]: after every
/// chunk's own entries.
pub(crate) const OWN_FIRST: usize = OWN_ENTRIES;

/// Every chunk's own entries, chunk by chunk, then the library's own
/// routines; and each entry's routine among them.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a routine's index, fewer than a u16 counts as asserted"
)]
const LAYOUT: (
    [Code; OWN_FIRST + crate::kernel::OWN.len()],
    [u16; ENTRY_COUNT],
) = {
    assert!(
        OWN_FIRST + crate::kernel::OWN.len() <= 1 << u16::BITS,
        "every routine's index a u16"
    );
    let own = crate::kernel::OWN;
    let mut routines = [own[0]; OWN_FIRST + crate::kernel::OWN.len()];
    let mut slots = [0u16; ENTRY_COUNT];
    let mut next = 0;
    let mut n = 0;
    while n < CHUNKS.len() {
        let mut e = 0;
        while e < CHUNKS[n].entries.len() {
            slots[FIRSTS[n] + e] = match CHUNKS[n].entries[e] {
                Entry::Own(code) => {
                    routines[next] = code;
                    next += 1;
                    (next - 1) as u16
                }
                Entry::Shared { chunk, entry } => {
                    let from = position(chunk);
                    assert!(
                        from < n || (from == n && entry < e),
                        "shared only with an entry before it"
                    );
                    assert!(
                        matches!(CHUNKS[from].entries[entry], Entry::Own(_)),
                        "shared with an own entry"
                    );
                    slots[FIRSTS[from] + entry]
                }
            };
            e += 1;
        }
        n += 1;
    }
    let mut r = 0;
    while r < own.len() {
        routines[OWN_FIRST + r] = own[r];
        r += 1;
    }
    (routines, slots)
};

/// The module's routines: every chunk's own entries, then the library's.
pub(crate) static ROUTINES: [Code; OWN_FIRST + crate::kernel::OWN.len()] = LAYOUT.0;

/// The routine among [`ROUTINES`] that `chunk`'s `entry` is.
pub(crate) fn routine(chunk: &Chunk, entry: usize) -> Option<usize> {
    let n = CHUNKS.iter().position(|c| c.id == chunk.id)?;
    (entry < chunk.entries()).then(|| usize::from(LAYOUT.1[FIRSTS[n] + entry]))
}

/// The routine among [`ROUTINES`] that the C library's entry `entry` is.
pub(crate) const fn c_routine(entry: usize) -> usize {
    LAYOUT.1[FIRSTS[position(2)] + entry] as usize
}

/// The library statics' template: each chunk's part as a client's copy
/// starts, all nought — the kernel's fields are laid by `_kernel_init`.
const STATICS: u32 = statics_end();

/// Where the last built chunk's statics end.
const fn statics_end() -> u32 {
    let mut end = 0;
    let mut n = 0;
    while n < CHUNKS.len() {
        let chunk_end = CHUNKS[n].statics_at + CHUNKS[n].statics;
        if chunk_end > end {
            end = chunk_end;
        }
        n += 1;
    }
    end
}

/// The saved PC of a frame record the library writes during stack
/// extension, from the guest code's start: just past a frame-storing
/// `STMFD SP!, {FP, IP, LR, PC}`, wherever a reader looks for the
/// instruction that stored it.
pub(crate) const RECORD_PC: u32 = STATICS + 4 * FRAME_STORES + 4;

/// The frame-storing instructions after the template: one where a core
/// that stores PC plus 8 put it, one where an older core's plus 12 did.
const FRAME_STORES: u32 = 2;

/// The string `_clib_version` answers, from the guest code's start: after
/// the frame-storing instructions. The version is the library's RISC OS
/// 5.30 answers to.
pub(crate) const VERSION_AT: u32 = STATICS + 4 * FRAME_STORES;
const VERSION: [u8; VERSION_LEN] = version_text();
const VERSION_PREFIX: &[u8] = b"TAIRiX Bridge SharedCLibrary vsn ";
const VERSION_SUFFIX: &[u8] = b"/32\n\0";
const VERSION_LEN: usize = VERSION_PREFIX.len() + 3 + VERSION_SUFFIX.len();

/// The version string, its three digits the module's inventory row's.
#[expect(clippy::cast_possible_truncation, reason = "a decimal digit")]
const fn version_text() -> [u8; VERSION_LEN] {
    let version = tairix_riscos_kernel::module::inventory::SHARED_C_LIBRARY_ROM.version;
    assert!(
        100 <= version && version < 1000,
        "a version of three digits"
    );
    let digits = [version / 100, version / 10 % 10, version % 10];
    let mut text = [0; VERSION_LEN];
    let mut at = 0;
    while at < VERSION_LEN {
        let prefixed = VERSION_PREFIX.len();
        text[at] = if at < prefixed {
            VERSION_PREFIX[at]
        } else if at < prefixed + 3 {
            b'0' + digits[at - prefixed] as u8
        } else {
            VERSION_SUFFIX[at - prefixed - 3]
        };
        at += 1;
    }
    text
}

/// The module's guest code: the statics' template, which a client's library
/// static offset relocates the library's addresses from, the frame-storing
/// instructions the library's frame records point past, then the version
/// string.
pub(crate) fn emit(_first_routine: i32) -> Result<Vec<u32>, MemoryError> {
    let mut words = Vec::new();
    let template = STATICS as usize / 4;
    let stores = FRAME_STORES as usize;
    let version = VERSION.len().div_ceil(4);
    words.try_reserve_exact(template + stores + version)?;
    words.resize(template, 0);
    let store = instruction(Op::Block {
        op: BlockOp::Stm,
        rn: Reg::SP,
        mode: BlockMode::DecrementBefore,
        writeback: true,
        user: false,
        list: RegList(0b1101_1000_0000_0000),
    })?;
    words.resize(template + stores, store);
    words.extend(VERSION.chunks(4).map(|bytes| {
        let mut word = [0; 4];
        word[..bytes.len()].copy_from_slice(bytes);
        u32::from_le_bytes(word)
    }));
    Ok(words)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{chunk, emit, routine, Entry, CHUNKS, RECORD_PC, STATICS, VERSION_AT};

    #[test]
    fn the_guest_code_is_the_template_two_frame_storing_instructions_and_the_version() {
        let words = emit(0).expect("emitted");
        let template = STATICS as usize / 4;
        assert!(
            words[..template].iter().all(|&word| word == 0),
            "the template nought"
        );
        assert_eq!(
            words[template..template + 2],
            [0xE92D_D800; 2],
            "STMFD SP!, {{FP, IP, LR, PC}}"
        );
        assert_eq!(
            RECORD_PC,
            STATICS + 12,
            "past both, as a core storing PC plus 8 or 12 leaves it"
        );
        let bytes: alloc::vec::Vec<u8> = words[VERSION_AT as usize / 4..]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        assert!(bytes.starts_with(b"TAIRiX Bridge SharedCLibrary vsn 623/32\n\0"));
    }

    #[test]
    fn each_chunk_holds_as_many_entries_as_genuine_s() {
        let counts = CHUNKS.map(|c| (c.id, c.entries()));
        assert_eq!(
            counts,
            [
                (1, 48),
                (2, 187),
                (4, 50),
                (5, 189),
                (6, 40),
                (7, 63),
                (8, 50),
                (9, 25)
            ]
        );
    }

    #[test]
    fn a_shared_entry_is_the_routine_it_names() {
        for c in CHUNKS {
            for (n, entry) in c.entries.iter().enumerate() {
                if let Entry::Shared {
                    chunk: id,
                    entry: e,
                } = *entry
                {
                    let from = chunk(id).expect("a built chunk");
                    assert_eq!(
                        routine(&c, n),
                        routine(from, e),
                        "{}.{n} shares {id}.{e}",
                        c.id
                    );
                }
            }
        }
        let distinct: std::collections::BTreeSet<_> = CHUNKS
            .iter()
            .flat_map(|c| (0..c.entries()).filter_map(|n| routine(c, n)))
            .collect();
        assert_eq!(
            distinct.len(),
            super::OWN_FIRST,
            "every own entry a routine of its own"
        );
    }
}
