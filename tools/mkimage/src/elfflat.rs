//! ELF → flat aarch64 kernel image conversion.
//!
//! No loader the kernel boots from reads ELF: the Pi firmware copies
//! `kernel8.img`, and QEMU's `-kernel` an `Image`, each placing the bytes by
//! the arm64 `Image` header they open with and branching to their first
//! byte. The converter therefore takes the freestanding `tairix-kernel` ELF
//! (linked in the kernel's high half, loaded at a board's
//! [`KERNEL_LOAD_ADDR`] or [`VIRT_KERNEL_LOAD_ADDR`]) and lays its `PT_LOAD`
//! file bytes out at their physical offsets, zero-filling any inter-segment
//! gap. Trailing `.bss` is *not* emitted — the boot stub clears it — so the
//! image stays as small as the file-backed content.
//!
//! The header is the trampoline's own, laid down at `_start`; the converter
//! reads it back and refuses an image without one or one whose header would
//! have a loader put it anywhere but its load address, or zero memory past
//! what it states. Every other field that steers the layout is validated
//! fail-closed too: a kernel linked at the wrong address, an entry point that
//! is not the image start, or an overlapping or oversized segment is a build
//! defect the converter refuses rather than an image that faults on metal.

use tairix_arch_aarch64::image_header;

use crate::MkimageError;

/// Physical address the Pi firmware places `kernel8.img` at and enters it:
/// 2 MiB above the base of RAM, where it puts an image whose header's
/// `text_offset` is zero (`docs/src/platform/aarch64.md`, "Boot protocol").
/// A 2 MiB boundary, so the image's code maps with block leaves.
pub const KERNEL_LOAD_ADDR: u64 = 0x20_0000;

/// Physical address the QEMU `virt` board's kernel loads at
/// (`kernel/arch/aarch64/link/aarch64-virt.ld`): 2 MiB above the board's RAM
/// base, where QEMU's `-kernel` puts an image whose `text_offset` is zero.
pub const VIRT_KERNEL_LOAD_ADDR: u64 = 0x4020_0000;

/// Upper bound on the emitted flat image, in bytes. A defence bound against a malformed ELF demanding a huge zero-fill,
/// not a kernel-size capacity: the kernel is a few MiB; 64 MiB is far
/// beyond any honest layout.
pub const MAX_FLAT_BYTES: u64 = 64 << 20;

const ELF64_HEADER_LEN: usize = 64;
const ELF64_PHENTSIZE: usize = 56;
const ET_EXEC: u16 = 2;
const EM_AARCH64: u16 = 183;
const PT_LOAD: u32 = 1;

/// One `PT_LOAD` segment's file-backed extent.
struct LoadSeg {
    paddr: u64,
    filesz: u64,
    memsz: u64,
    offset: usize,
}

/// Convert the freestanding aarch64 kernel ELF `elf` into the flat image a
/// loader copies to `load_addr` and enters at its first byte (the Pi image
/// passes [`KERNEL_LOAD_ADDR`]; the QEMU `virt` image passes
/// [`VIRT_KERNEL_LOAD_ADDR`]).
///
/// # Errors
///
/// [`MkimageError::KernelElf`] if `elf` is not a statically-linked
/// little-endian `ELF64` aarch64 executable whose `PT_LOAD` layout starts
/// and enters at `load_addr`, if any segment is truncated, overlapping, or
/// beyond [`MAX_FLAT_BYTES`], or if the image does not open with an arm64
/// `Image` header that places it at `load_addr` and states its whole reach.
pub fn elf_to_flat(elf: &[u8], load_addr: u64) -> Result<Vec<u8>, MkimageError> {
    if elf.len() < ELF64_HEADER_LEN {
        return Err(MkimageError::KernelElf(
            "file too short for an ELF64 header",
        ));
    }
    if elf[..4] != [0x7f, b'E', b'L', b'F'] {
        return Err(MkimageError::KernelElf("not an ELF file"));
    }
    if elf[4] != 2 || elf[5] != 1 {
        return Err(MkimageError::KernelElf("not a little-endian ELF64 image"));
    }
    if read_u16(elf, 16)? != ET_EXEC {
        return Err(MkimageError::KernelElf(
            "not an ET_EXEC image (the kernel is statically linked, not PIE)",
        ));
    }
    if read_u16(elf, 18)? != EM_AARCH64 {
        return Err(MkimageError::KernelElf("not an aarch64 image"));
    }
    let entry = read_u64(elf, 24)?;
    let phoff = read_u64(elf, 32)?;
    let phentsize = usize::from(read_u16(elf, 54)?);
    let phnum = usize::from(read_u16(elf, 56)?);
    if phentsize != ELF64_PHENTSIZE {
        return Err(MkimageError::KernelElf("unexpected program-header size"));
    }

    let mut loads = decode_loads(elf, phoff, phnum)?;
    loads.sort_by_key(|s| s.paddr);
    let Some(first) = loads.first() else {
        return Err(MkimageError::KernelElf("no PT_LOAD segments"));
    };
    if first.paddr != load_addr {
        return Err(MkimageError::KernelElf(
            "kernel is not linked at the requested load address",
        ));
    }
    if entry != load_addr {
        return Err(MkimageError::KernelElf(
            "kernel entry point is not the image start (the loader branches to the first byte)",
        ));
    }
    for pair in loads.windows(2) {
        let prev_end = pair[0]
            .paddr
            .checked_add(pair[0].memsz)
            .ok_or(MkimageError::KernelElf("segment end overflows"))?;
        if pair[1].paddr < prev_end {
            return Err(MkimageError::KernelElf("PT_LOAD segments overlap"));
        }
    }

    // A segment with no file bytes is the kernel's to zero, so it neither
    // extends the image nor pads up to where it begins.
    let flat_len = loads
        .iter()
        .filter(|s| s.filesz != 0)
        .map(|s| s.paddr - load_addr + s.filesz)
        .max()
        .unwrap_or(0);
    let memory_len = loads
        .iter()
        .map(|s| s.paddr - load_addr + s.memsz)
        .max()
        .unwrap_or(0);
    if flat_len == 0 {
        return Err(MkimageError::KernelElf("image has no file-backed bytes"));
    }
    if flat_len > MAX_FLAT_BYTES {
        return Err(MkimageError::KernelElf("flat image exceeds the size bound"));
    }
    let flat_len =
        usize::try_from(flat_len).map_err(|_| MkimageError::KernelElf("image too large"))?;

    let mut flat = vec![0u8; flat_len];
    for seg in loads.iter().filter(|s| s.filesz != 0) {
        let filesz = usize::try_from(seg.filesz)
            .map_err(|_| MkimageError::KernelElf("segment too large"))?;
        let dst = usize::try_from(seg.paddr - load_addr)
            .map_err(|_| MkimageError::KernelElf("segment too far"))?;
        flat[dst..dst + filesz].copy_from_slice(&elf[seg.offset..seg.offset + filesz]);
    }
    check_image_header(&flat, memory_len)?;
    Ok(flat)
}

/// Refuse an image whose arm64 `Image` header is missing, would have a loader
/// place it anywhere but 2 MiB above the base of RAM, or states a reach other
/// than `memory_len`, the memory the kernel zeroes past its file included.
fn check_image_header(flat: &[u8], memory_len: u64) -> Result<(), MkimageError> {
    let header = image_header::header(flat).ok_or(MkimageError::KernelElf(
        "kernel image does not open with an arm64 Image header",
    ))?;
    if header.text_offset != 0 {
        return Err(MkimageError::KernelElf(
            "kernel image header names a text_offset, so a loader places it off its load address",
        ));
    }
    if header.flags != image_header::FLAGS_LE_4K {
        return Err(MkimageError::KernelElf(
            "kernel image header does not state a little-endian kernel with 4 KiB pages",
        ));
    }
    if header.image_size != memory_len {
        return Err(MkimageError::KernelElf(
            "kernel image header image_size is not the kernel's reach",
        ));
    }
    Ok(())
}

/// Decode and bound-check every `PT_LOAD` program header.
fn decode_loads(elf: &[u8], phoff: u64, phnum: usize) -> Result<Vec<LoadSeg>, MkimageError> {
    let phoff =
        usize::try_from(phoff).map_err(|_| MkimageError::KernelElf("program headers truncated"))?;
    let mut loads = Vec::new();
    for i in 0..phnum {
        let at = phoff
            .checked_add(i * ELF64_PHENTSIZE)
            .ok_or(MkimageError::KernelElf("program headers truncated"))?;
        if read_u32(elf, at)? != PT_LOAD {
            continue;
        }
        let offset = read_u64(elf, at + 8)?;
        let paddr = read_u64(elf, at + 24)?;
        let filesz = read_u64(elf, at + 32)?;
        let memsz = read_u64(elf, at + 40)?;
        if filesz > memsz {
            return Err(MkimageError::KernelElf(
                "segment file size exceeds memory size",
            ));
        }
        let offset = usize::try_from(offset)
            .map_err(|_| MkimageError::KernelElf("segment offset out of range"))?;
        let file_end = u64::try_from(offset)
            .ok()
            .and_then(|o| o.checked_add(filesz))
            .ok_or(MkimageError::KernelElf("segment extent overflows"))?;
        if file_end > elf.len() as u64 {
            return Err(MkimageError::KernelElf("segment extends past the file"));
        }
        if filesz == 0 && memsz == 0 {
            continue;
        }
        loads.push(LoadSeg {
            paddr,
            filesz,
            memsz,
            offset,
        });
    }
    Ok(loads)
}

fn read_u16(bytes: &[u8], at: usize) -> Result<u16, MkimageError> {
    let end = at
        .checked_add(2)
        .ok_or(MkimageError::KernelElf("truncated field"))?;
    let slice = bytes
        .get(at..end)
        .ok_or(MkimageError::KernelElf("truncated field"))?;
    Ok(u16::from_le_bytes(
        slice
            .try_into()
            .map_err(|_| MkimageError::KernelElf("truncated field"))?,
    ))
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32, MkimageError> {
    let end = at
        .checked_add(4)
        .ok_or(MkimageError::KernelElf("truncated field"))?;
    let slice = bytes
        .get(at..end)
        .ok_or(MkimageError::KernelElf("truncated field"))?;
    Ok(u32::from_le_bytes(
        slice
            .try_into()
            .map_err(|_| MkimageError::KernelElf("truncated field"))?,
    ))
}

fn read_u64(bytes: &[u8], at: usize) -> Result<u64, MkimageError> {
    let end = at
        .checked_add(8)
        .ok_or(MkimageError::KernelElf("truncated field"))?;
    let slice = bytes
        .get(at..end)
        .ok_or(MkimageError::KernelElf("truncated field"))?;
    Ok(u64::from_le_bytes(
        slice
            .try_into()
            .map_err(|_| MkimageError::KernelElf("truncated field"))?,
    ))
}

/// Test-only builders shared with the crate-level assembly tests.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::{
        image_header, ELF64_HEADER_LEN, ELF64_PHENTSIZE, EM_AARCH64, ET_EXEC, KERNEL_LOAD_ADDR,
        PT_LOAD,
    };

    pub fn w16(buf: &mut [u8], at: usize, x: u16) {
        buf[at..at + 2].copy_from_slice(&x.to_le_bytes());
    }
    pub fn w32(buf: &mut [u8], at: usize, x: u32) {
        buf[at..at + 4].copy_from_slice(&x.to_le_bytes());
    }
    pub fn w64(buf: &mut [u8], at: usize, x: u64) {
        buf[at..at + 8].copy_from_slice(&x.to_le_bytes());
    }

    pub struct Seg {
        pub paddr: u64,
        pub bytes: Vec<u8>,
        pub bss: u64,
    }

    /// The arm64 `Image` header the trampoline opens an image with, stating
    /// `image_size`.
    pub fn image_header(image_size: u64) -> Vec<u8> {
        let mut header = vec![0u8; image_header::HEADER_LEN];
        w64(&mut header, 16, image_size);
        w64(&mut header, 24, image_header::FLAGS_LE_4K);
        w32(&mut header, 56, image_header::MAGIC);
        header
    }

    /// `code` behind the header of an image reaching `extra` bytes past it.
    pub fn headed(code: &[u8], extra: u64) -> Vec<u8> {
        let mut bytes = image_header((image_header::HEADER_LEN + code.len()) as u64 + extra);
        bytes.extend_from_slice(code);
        bytes
    }

    /// A minimal valid kernel ELF: one code segment of `code`, behind its
    /// header, at the Pi load address, entered at its first byte.
    pub fn sample_kernel(code: &[u8]) -> Vec<u8> {
        sample_elf(
            KERNEL_LOAD_ADDR,
            &[Seg {
                paddr: KERNEL_LOAD_ADDR,
                bytes: headed(code, 0),
                bss: 0,
            }],
        )
    }

    /// A minimal `ET_EXEC` aarch64 ELF with the given segments, entered at
    /// `entry`.
    pub fn sample_elf(entry: u64, segs: &[Seg]) -> Vec<u8> {
        let phnum = segs.len();
        let data_at = ELF64_HEADER_LEN + phnum * ELF64_PHENTSIZE;
        let mut out = vec![0u8; data_at];
        out[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        out[4] = 2; // ELFCLASS64
        out[5] = 1; // little-endian
        out[6] = 1; // EV_CURRENT
        w16(&mut out, 16, ET_EXEC);
        w16(&mut out, 18, EM_AARCH64);
        w32(&mut out, 20, 1);
        w64(&mut out, 24, entry);
        w64(&mut out, 32, ELF64_HEADER_LEN as u64); // e_phoff
        w16(&mut out, 52, u16::try_from(ELF64_HEADER_LEN).expect("fits")); // e_ehsize
        w16(&mut out, 54, u16::try_from(ELF64_PHENTSIZE).expect("fits"));
        w16(&mut out, 56, u16::try_from(phnum).expect("fits"));

        let mut offset = data_at as u64;
        for (i, seg) in segs.iter().enumerate() {
            let at = ELF64_HEADER_LEN + i * ELF64_PHENTSIZE;
            w32(&mut out, at, PT_LOAD);
            w32(&mut out, at + 4, 0b101); // R+X; the converter ignores flags
            w64(&mut out, at + 8, offset);
            w64(&mut out, at + 16, seg.paddr); // p_vaddr
            w64(&mut out, at + 24, seg.paddr); // p_paddr
            w64(&mut out, at + 32, seg.bytes.len() as u64);
            w64(&mut out, at + 40, seg.bytes.len() as u64 + seg.bss);
            w64(&mut out, at + 48, 16);
            offset += seg.bytes.len() as u64;
        }
        for seg in segs {
            out.extend_from_slice(&seg.bytes);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::{headed, image_header, sample_elf, sample_kernel, w16, w64, Seg};
    use super::*;

    /// The header's length: the first byte of a sample's own code.
    const AFTER_HEADER: u64 = image_header::HEADER_LEN as u64;

    #[test]
    fn a_virt_kernel_converts_at_its_own_load_address() {
        let code = [0xde, 0xad, 0xbe, 0xef];
        let elf = sample_elf(
            VIRT_KERNEL_LOAD_ADDR,
            &[Seg {
                paddr: VIRT_KERNEL_LOAD_ADDR,
                bytes: headed(&code, 0),
                bss: 0,
            }],
        );
        let image = elf_to_flat(&elf, VIRT_KERNEL_LOAD_ADDR).expect("valid virt kernel converts");
        assert_eq!(&image[image_header::HEADER_LEN..], &code);
        assert!(
            elf_to_flat(&elf, KERNEL_LOAD_ADDR).is_err(),
            "a virt kernel is no Pi image"
        );
    }

    #[test]
    fn lays_segments_out_at_their_physical_offsets() {
        let elf = sample_elf(
            KERNEL_LOAD_ADDR,
            &[
                Seg {
                    paddr: KERNEL_LOAD_ADDR,
                    bytes: headed(&[0x11; 8], 8 + 4 + 32),
                    bss: 0,
                },
                Seg {
                    paddr: KERNEL_LOAD_ADDR + AFTER_HEADER + 16,
                    bytes: vec![0x22; 4],
                    bss: 32,
                },
            ],
        );
        let flat = elf_to_flat(&elf, KERNEL_LOAD_ADDR).expect("valid kernel converts");
        // The header, 8 code bytes, an 8-byte zero gap, 4 data bytes; bss not
        // emitted.
        let code = image_header::HEADER_LEN;
        assert_eq!(flat.len(), code + 20);
        assert_eq!(&flat[code..code + 8], &[0x11; 8]);
        assert_eq!(&flat[code + 8..code + 16], &[0u8; 8]);
        assert_eq!(&flat[code + 16..], &[0x22; 4]);
    }

    /// The early tables are a no-load segment of their own past `.bss`: the
    /// image still ends with its last file byte, not where they begin, and
    /// its header states the reach the tables end.
    fn kernel_with_a_no_load_tail(image_size: u64) -> Vec<u8> {
        let load = KERNEL_LOAD_ADDR;
        let mut head = image_header(image_size);
        head.extend_from_slice(&[0x11; 8]);
        sample_elf(
            load,
            &[
                Seg {
                    paddr: load,
                    bytes: head,
                    bss: 0,
                },
                Seg {
                    paddr: load + AFTER_HEADER + 8,
                    bytes: vec![0x22; 4],
                    bss: 0x1000,
                },
                Seg {
                    paddr: load + 0x2000,
                    bytes: Vec::new(),
                    bss: 0x3000,
                },
            ],
        )
    }

    #[test]
    fn a_no_load_segment_past_the_bss_adds_no_bytes() {
        let flat = elf_to_flat(&kernel_with_a_no_load_tail(0x5000), KERNEL_LOAD_ADDR)
            .expect("valid kernel converts");
        assert_eq!(flat.len(), image_header::HEADER_LEN + 12);
        assert_eq!(&flat[image_header::HEADER_LEN + 8..], &[0x22; 4]);
    }

    #[test]
    fn the_header_states_the_reach_its_no_load_tail_ends() {
        for wrong in [image_header::HEADER_LEN as u64 + 12, 0x5000 - 1, 0] {
            assert!(
                elf_to_flat(&kernel_with_a_no_load_tail(wrong), KERNEL_LOAD_ADDR).is_err(),
                "image_size {wrong:#x}"
            );
        }
    }

    #[test]
    fn an_image_without_its_header_is_refused() {
        let elf = sample_elf(
            KERNEL_LOAD_ADDR,
            &[Seg {
                paddr: KERNEL_LOAD_ADDR,
                bytes: vec![0x11; 80],
                bss: 0,
            }],
        );
        assert!(elf_to_flat(&elf, KERNEL_LOAD_ADDR).is_err());
    }

    #[test]
    fn a_header_a_loader_would_misplace_by_is_refused() {
        let elf = |offset: usize, value: u64| {
            let mut bytes = headed(&[0x11; 8], 0);
            w64(&mut bytes, offset, value);
            sample_elf(
                KERNEL_LOAD_ADDR,
                &[Seg {
                    paddr: KERNEL_LOAD_ADDR,
                    bytes,
                    bss: 0,
                }],
            )
        };
        assert!(
            elf_to_flat(&elf(8, KERNEL_LOAD_ADDR), KERNEL_LOAD_ADDR).is_err(),
            "text_offset"
        );
        assert!(
            elf_to_flat(&elf(24, 0b100), KERNEL_LOAD_ADDR).is_err(),
            "16 KiB pages"
        );
        assert!(
            elf_to_flat(&elf(24, 0b011), KERNEL_LOAD_ADDR).is_err(),
            "big-endian"
        );
        assert!(elf_to_flat(&sample_kernel(&[0x11; 8]), KERNEL_LOAD_ADDR).is_ok());
    }

    #[test]
    fn rejects_non_elf_and_truncated_input() {
        assert!(elf_to_flat(b"not an elf", KERNEL_LOAD_ADDR).is_err());
        let elf = sample_elf(
            KERNEL_LOAD_ADDR,
            &[Seg {
                paddr: KERNEL_LOAD_ADDR,
                bytes: vec![0x11; 8],
                bss: 0,
            }],
        );
        assert!(elf_to_flat(&elf[..elf.len() - 4], KERNEL_LOAD_ADDR).is_err());
    }

    #[test]
    fn rejects_wrong_class_machine_and_type() {
        let good = sample_elf(
            KERNEL_LOAD_ADDR,
            &[Seg {
                paddr: KERNEL_LOAD_ADDR,
                bytes: vec![0x11; 8],
                bss: 0,
            }],
        );
        let mut wrong_class = good.clone();
        wrong_class[4] = 1;
        assert!(elf_to_flat(&wrong_class, KERNEL_LOAD_ADDR).is_err());

        let mut wrong_machine = good.clone();
        w16(&mut wrong_machine, 18, 0x3e); // EM_X86_64
        assert!(elf_to_flat(&wrong_machine, KERNEL_LOAD_ADDR).is_err());

        let mut pie = good;
        w16(&mut pie, 16, 3); // ET_DYN
        assert!(elf_to_flat(&pie, KERNEL_LOAD_ADDR).is_err());
    }

    #[test]
    fn rejects_wrong_load_address_and_entry() {
        let wrong_base = sample_elf(
            0x4020_0000,
            &[Seg {
                paddr: 0x4020_0000,
                bytes: vec![0x11; 8],
                bss: 0,
            }],
        );
        assert!(elf_to_flat(&wrong_base, KERNEL_LOAD_ADDR).is_err());

        let wrong_entry = sample_elf(
            KERNEL_LOAD_ADDR + 4,
            &[Seg {
                paddr: KERNEL_LOAD_ADDR,
                bytes: vec![0x11; 8],
                bss: 0,
            }],
        );
        assert!(elf_to_flat(&wrong_entry, KERNEL_LOAD_ADDR).is_err());
    }

    #[test]
    fn rejects_overlapping_segments() {
        let elf = sample_elf(
            KERNEL_LOAD_ADDR,
            &[
                Seg {
                    paddr: KERNEL_LOAD_ADDR,
                    bytes: vec![0x11; 8],
                    bss: 32,
                },
                Seg {
                    paddr: KERNEL_LOAD_ADDR + 8,
                    bytes: vec![0x22; 4],
                    bss: 0,
                },
            ],
        );
        assert!(elf_to_flat(&elf, KERNEL_LOAD_ADDR).is_err());
    }

    #[test]
    fn rejects_an_image_beyond_the_size_bound() {
        // A one-byte tail placed past the bound forces a huge zero-fill.
        let elf = sample_elf(
            KERNEL_LOAD_ADDR,
            &[
                Seg {
                    paddr: KERNEL_LOAD_ADDR,
                    bytes: vec![0x11; 8],
                    bss: 0,
                },
                Seg {
                    paddr: KERNEL_LOAD_ADDR + MAX_FLAT_BYTES,
                    bytes: vec![0x22; 4],
                    bss: 0,
                },
            ],
        );
        assert!(elf_to_flat(&elf, KERNEL_LOAD_ADDR).is_err());
    }

    #[test]
    fn rejects_filesz_beyond_memsz_and_truncated_segment() {
        let mut bad_sz = sample_elf(
            KERNEL_LOAD_ADDR,
            &[Seg {
                paddr: KERNEL_LOAD_ADDR,
                bytes: vec![0x11; 8],
                bss: 0,
            }],
        );
        // p_memsz < p_filesz
        w64(&mut bad_sz, ELF64_HEADER_LEN + 40, 4);
        assert!(elf_to_flat(&bad_sz, KERNEL_LOAD_ADDR).is_err());

        let mut past_eof = sample_elf(
            KERNEL_LOAD_ADDR,
            &[Seg {
                paddr: KERNEL_LOAD_ADDR,
                bytes: vec![0x11; 8],
                bss: 0,
            }],
        );
        // p_filesz/p_memsz reach past the end of the file.
        w64(&mut past_eof, ELF64_HEADER_LEN + 32, 1 << 20);
        w64(&mut past_eof, ELF64_HEADER_LEN + 40, 1 << 20);
        assert!(elf_to_flat(&past_eof, KERNEL_LOAD_ADDR).is_err());
    }
}
