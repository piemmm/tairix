//! U-mode fixture: fill the whole floating-point (and, on x86_64, x87) register
//! file with a per-task pattern, trap into the kernel, and check every register
//! came back.
//!
//! The register file is task state the kernel must switch per task, or two
//! tasks share one physical file — each reading whatever the last one left in
//! it, which is disclosure as much as corruption
//! (`plans/OPEN-DEFECTS.md` D37/D359). The consuming vertical
//! (`fp_isolation_qemu_<arch>`) runs two copies of this program with different
//! seeds interleaved on one CPU; it passes only if neither sees the other's
//! values.
//!
//! Each arch's load, trap and read-back is **one** asm block on purpose. Half
//! the vector registers are caller-saved, so a Rust call between them would let
//! the compiler treat the values as dead, and the test could pass without ever
//! having held them across the trap.
//!
//! It is a **pure-Rust** program: it links `tairix-rt` (`_start`, the stack
//! canary, the panic handler, the syscall wrappers), never the C ABI. On the
//! host, and on any non-freestanding target, it is an inert stub so
//! `cargo build --workspace`, clippy and fmt still cover the crate.

#![cfg_attr(fp_probe, no_std)]
#![cfg_attr(fp_probe, no_main)]
#![deny(missing_docs)]

#[cfg(fp_probe)]
mod program {
    use tairix_abi::SyscallNumber;

    /// Doublewords the probe holds across a trap: one per vector/FP register,
    /// plus the control word and, on x86_64, the x87 stack.
    #[cfg(fp_probe_x86_64)]
    const SLOTS: usize = 16 + 1 + 8; // xmm0-15, MXCSR, st0-7
    #[cfg(fp_probe_aarch64)]
    const SLOTS: usize = 32 + 1; // q0-31 (low 64 bits each), FPCR
    #[cfg(fp_probe_riscv64)]
    const SLOTS: usize = 32 + 1; // f0-31, fcsr

    /// Trips the pattern apart per slot, so a save/restore that transposed,
    /// truncated, or only partly covered the file fails rather than passing on
    /// a coincidence.
    const SPREAD: u64 = 0x9E37_79B9_7F4A_7C15;

    /// Exit code for a slot that came back holding something else.
    const EXIT_FILE_CLOBBERED: i32 = 72;
    /// Exit code for a missing or unusable per-task seed argument.
    const EXIT_NO_SEED: i32 = 73;

    /// Rounds of fill-trap-verify when the consuming build pinned no count.
    /// More than one so a port that happened to preserve the file across the
    /// *first* trap still fails.
    const DEFAULT_ROUNDS: u32 = 4;

    /// The round count, read from the `TAIRIX_FP_ROUNDS` environment variable
    /// the consuming vertical's build script sets. That script emits the same
    /// number as a Rust constant for its kernel side, so it is the single
    /// source of truth for the yield count the vertical asserts against.
    const fn rounds() -> u32 {
        match option_env!("TAIRIX_FP_ROUNDS") {
            Some(text) => parse_u32(text.as_bytes()),
            None => DEFAULT_ROUNDS,
        }
    }

    /// Parse `bytes` as a non-negative decimal integer at compile time,
    /// falling back to [`DEFAULT_ROUNDS`]. `const` and panic-free.
    const fn parse_u32(bytes: &[u8]) -> u32 {
        let mut acc: u32 = 0;
        let mut index = 0usize;
        let mut seen = false;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte < b'0' || byte > b'9' {
                return DEFAULT_ROUNDS;
            }
            acc = match acc.checked_mul(10) {
                Some(scaled) => match scaled.checked_add((byte - b'0') as u32) {
                    Some(next) => next,
                    None => return DEFAULT_ROUNDS,
                },
                None => return DEFAULT_ROUNDS,
            };
            seen = true;
            index += 1;
        }
        if seen {
            acc
        } else {
            DEFAULT_ROUNDS
        }
    }

    /// The value slot `index` carries for this task in `round`.
    ///
    /// Every pattern is a quiet-NaN payload rather than a signalling encoding:
    /// the fixture only moves bits, and a trap on a signalling NaN would be
    /// this test's own bug rather than the kernel's. The control-word slot is
    /// masked to a value each control register accepts (see the arch bodies),
    /// so filling it never sets a reserved bit.
    fn pattern(seed: u64, round: u64, index: usize) -> u64 {
        let index = index as u64;
        let base = 0x7FF8_0000_0000_0000 | (seed << 32) | (round << 16) | (index & 0xFFFF);
        base ^ (SPREAD.wrapping_mul(index + 1) & 0x0000_FFFF_FFFF_0000)
    }

    /// x86_64: fill `xmm0`–`xmm15` (low 64 bits), `MXCSR`, and the x87 stack
    /// from `write`; yield; read them back into `read`. The `MXCSR` slot sets a
    /// non-default rounding mode and unmasks an exception, which the kernel's
    /// own floating point must not observe and which the register must carry
    /// back unchanged.
    #[cfg(fp_probe_x86_64)]
    fn hold_across_trap(write: &[u64; SLOTS], read: &mut [u64; SLOTS]) {
        // Slot layout: [0..16] xmm, [16] MXCSR, [17..25] x87 st0-7.
        let mut mxcsr = [0u32; 1];
        // Round toward zero (bits 13:14 = 11) with the invalid-op exception
        // unmasked (bit 7 clear); every other masked. A legal MXCSR the kernel
        // must not run under and must hand back intact.
        mxcsr[0] = 0x1F80 & !(1 << 7) | (0b11 << 13);
        let number = u64::from(SyscallNumber::YIELD.as_u16());
        // SAFETY: `write`/`read` are `SLOTS`-doubleword buffers the caller owns.
        // `syscall` is the `abi-v1` trap; `yield` is unprivileged and resumes
        // at the next instruction. Every `xmm`, the x87 stack and `MXCSR` are
        // declared clobbered, so the block owns the whole file across the trap.
        // The x87 loads run after `finit` gives a known stack; `fldl` pushes,
        // so st0..st7 are loaded in reverse to land the array in order.
        unsafe {
            core::arch::asm!(
                "movsd xmm0, qword ptr [{w} + 0]",
                "movsd xmm1, qword ptr [{w} + 8]",
                "movsd xmm2, qword ptr [{w} + 16]",
                "movsd xmm3, qword ptr [{w} + 24]",
                "movsd xmm4, qword ptr [{w} + 32]",
                "movsd xmm5, qword ptr [{w} + 40]",
                "movsd xmm6, qword ptr [{w} + 48]",
                "movsd xmm7, qword ptr [{w} + 56]",
                "movsd xmm8, qword ptr [{w} + 64]",
                "movsd xmm9, qword ptr [{w} + 72]",
                "movsd xmm10, qword ptr [{w} + 80]",
                "movsd xmm11, qword ptr [{w} + 88]",
                "movsd xmm12, qword ptr [{w} + 96]",
                "movsd xmm13, qword ptr [{w} + 104]",
                "movsd xmm14, qword ptr [{w} + 112]",
                "movsd xmm15, qword ptr [{w} + 120]",
                "finit",
                "fld qword ptr [{w} + 192]",
                "fld qword ptr [{w} + 184]",
                "fld qword ptr [{w} + 176]",
                "fld qword ptr [{w} + 168]",
                "fld qword ptr [{w} + 160]",
                "fld qword ptr [{w} + 152]",
                "fld qword ptr [{w} + 144]",
                "fld qword ptr [{w} + 136]",
                "ldmxcsr [{mx}]",
                "syscall",
                "stmxcsr [{mx}]",
                "movsd qword ptr [{r} + 0], xmm0",
                "movsd qword ptr [{r} + 8], xmm1",
                "movsd qword ptr [{r} + 16], xmm2",
                "movsd qword ptr [{r} + 24], xmm3",
                "movsd qword ptr [{r} + 32], xmm4",
                "movsd qword ptr [{r} + 40], xmm5",
                "movsd qword ptr [{r} + 48], xmm6",
                "movsd qword ptr [{r} + 56], xmm7",
                "movsd qword ptr [{r} + 64], xmm8",
                "movsd qword ptr [{r} + 72], xmm9",
                "movsd qword ptr [{r} + 80], xmm10",
                "movsd qword ptr [{r} + 88], xmm11",
                "movsd qword ptr [{r} + 96], xmm12",
                "movsd qword ptr [{r} + 104], xmm13",
                "movsd qword ptr [{r} + 112], xmm14",
                "movsd qword ptr [{r} + 120], xmm15",
                "fstp qword ptr [{r} + 136]",
                "fstp qword ptr [{r} + 144]",
                "fstp qword ptr [{r} + 152]",
                "fstp qword ptr [{r} + 160]",
                "fstp qword ptr [{r} + 168]",
                "fstp qword ptr [{r} + 176]",
                "fstp qword ptr [{r} + 184]",
                "fstp qword ptr [{r} + 192]",
                w = in(reg) write.as_ptr(),
                r = in(reg) read.as_mut_ptr(),
                mx = in(reg) mxcsr.as_ptr(),
                inout("rax") number => _,
                out("rcx") _, out("r11") _,
                out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
                out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
                out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
                out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
                options(nostack),
            );
        }
        read[16] = u64::from(mxcsr[0]);
    }

    /// x86_64 fills the `MXCSR` slot itself (a masked value), so the pattern's
    /// slot 16 is ignored; make the two agree before the comparison.
    #[cfg(fp_probe_x86_64)]
    fn normalise(write: &mut [u64; SLOTS]) {
        write[16] = u64::from(0x1F80u32 & !(1 << 7) | (0b11 << 13));
        // The x87 slots hold 80-bit registers narrowed to 64-bit doubles on
        // store, so only a value that round-trips through f64 may be compared.
        for slot in &mut write[17..25] {
            *slot = f64::from_bits(*slot).to_bits();
        }
    }

    /// aarch64: fill `q0`–`q31` (low 64 bits) and `FPCR` from `write`; yield;
    /// read back. The `FPCR` slot sets round-toward-zero and flush-to-zero, a
    /// legal control value the kernel must not run under and must restore.
    #[cfg(fp_probe_aarch64)]
    fn hold_across_trap(write: &[u64; SLOTS], read: &mut [u64; SLOTS]) {
        let number = u64::from(SyscallNumber::YIELD.as_u16());
        // SAFETY: `write`/`read` are `SLOTS`-doubleword buffers the caller
        // owns. `svc #0` is the `abi-v1` trap; `yield` resumes at the next
        // instruction. Every `q` register and `FPCR`/`FPSR` are declared
        // clobbered.
        unsafe {
            core::arch::asm!(
                "ldp d0, d1, [{w}, #0]",
                "ldp d2, d3, [{w}, #16]",
                "ldp d4, d5, [{w}, #32]",
                "ldp d6, d7, [{w}, #48]",
                "ldp d8, d9, [{w}, #64]",
                "ldp d10, d11, [{w}, #80]",
                "ldp d12, d13, [{w}, #96]",
                "ldp d14, d15, [{w}, #112]",
                "ldp d16, d17, [{w}, #128]",
                "ldp d18, d19, [{w}, #144]",
                "ldp d20, d21, [{w}, #160]",
                "ldp d22, d23, [{w}, #176]",
                "ldp d24, d25, [{w}, #192]",
                "ldp d26, d27, [{w}, #208]",
                "ldp d28, d29, [{w}, #224]",
                "ldp d30, d31, [{w}, #240]",
                "ldr {t}, [{w}, #256]",
                "msr fpcr, {t}",
                "svc #0",
                "mrs {t}, fpcr",
                "str {t}, [{r}, #256]",
                "stp d0, d1, [{r}, #0]",
                "stp d2, d3, [{r}, #16]",
                "stp d4, d5, [{r}, #32]",
                "stp d6, d7, [{r}, #48]",
                "stp d8, d9, [{r}, #64]",
                "stp d10, d11, [{r}, #80]",
                "stp d12, d13, [{r}, #96]",
                "stp d14, d15, [{r}, #112]",
                "stp d16, d17, [{r}, #128]",
                "stp d18, d19, [{r}, #144]",
                "stp d20, d21, [{r}, #160]",
                "stp d22, d23, [{r}, #176]",
                "stp d24, d25, [{r}, #192]",
                "stp d26, d27, [{r}, #208]",
                "stp d28, d29, [{r}, #224]",
                "stp d30, d31, [{r}, #240]",
                w = in(reg) write.as_ptr(),
                r = in(reg) read.as_mut_ptr(),
                t = out(reg) _,
                in("x8") number,
                out("x0") _,
                out("d0") _, out("d1") _, out("d2") _, out("d3") _, out("d4") _, out("d5") _,
                out("d6") _, out("d7") _, out("d8") _, out("d9") _, out("d10") _, out("d11") _,
                out("d12") _, out("d13") _, out("d14") _, out("d15") _, out("d16") _, out("d17") _,
                out("d18") _, out("d19") _, out("d20") _, out("d21") _, out("d22") _, out("d23") _,
                out("d24") _, out("d25") _, out("d26") _, out("d27") _, out("d28") _, out("d29") _,
                out("d30") _, out("d31") _,
                options(nostack),
            );
        }
    }

    /// aarch64 fills `FPCR` with a masked control value, so the pattern's
    /// slot 32 is replaced before the comparison.
    #[cfg(fp_probe_aarch64)]
    fn normalise(write: &mut [u64; SLOTS]) {
        // RMode = round toward zero (bits 23:22 = 11), FZ (bit 24). A legal
        // control the kernel must not adopt.
        write[32] = (0b11 << 22) | (1 << 24);
    }

    /// riscv64: fill `f0`–`f31` and `fcsr` from `write`; yield; read back.
    #[cfg(fp_probe_riscv64)]
    fn hold_across_trap(write: &[u64; SLOTS], read: &mut [u64; SLOTS]) {
        let number = u64::from(SyscallNumber::YIELD.as_u16());
        // SAFETY: both operands are `SLOTS`-doubleword buffers the caller owns.
        // `ecall` is the `abi-v1` trap; `yield` resumes at the next
        // instruction. `f0`–`f31` are declared clobbered.
        unsafe {
            core::arch::asm!(
                "fld f0, 0({w})", "fld f1, 8({w})", "fld f2, 16({w})", "fld f3, 24({w})",
                "fld f4, 32({w})", "fld f5, 40({w})", "fld f6, 48({w})", "fld f7, 56({w})",
                "fld f8, 64({w})", "fld f9, 72({w})", "fld f10, 80({w})", "fld f11, 88({w})",
                "fld f12, 96({w})", "fld f13, 104({w})", "fld f14, 112({w})", "fld f15, 120({w})",
                "fld f16, 128({w})", "fld f17, 136({w})", "fld f18, 144({w})", "fld f19, 152({w})",
                "fld f20, 160({w})", "fld f21, 168({w})", "fld f22, 176({w})", "fld f23, 184({w})",
                "fld f24, 192({w})", "fld f25, 200({w})", "fld f26, 208({w})", "fld f27, 216({w})",
                "fld f28, 224({w})", "fld f29, 232({w})", "fld f30, 240({w})", "fld f31, 248({w})",
                "ld {t}, 256({w})",
                "fscsr {t}",
                "ecall",
                "frcsr {t}",
                "sd {t}, 256({r})",
                "fsd f0, 0({r})", "fsd f1, 8({r})", "fsd f2, 16({r})", "fsd f3, 24({r})",
                "fsd f4, 32({r})", "fsd f5, 40({r})", "fsd f6, 48({r})", "fsd f7, 56({r})",
                "fsd f8, 64({r})", "fsd f9, 72({r})", "fsd f10, 80({r})", "fsd f11, 88({r})",
                "fsd f12, 96({r})", "fsd f13, 104({r})", "fsd f14, 112({r})", "fsd f15, 120({r})",
                "fsd f16, 128({r})", "fsd f17, 136({r})", "fsd f18, 144({r})", "fsd f19, 152({r})",
                "fsd f20, 160({r})", "fsd f21, 168({r})", "fsd f22, 176({r})", "fsd f23, 184({r})",
                "fsd f24, 192({r})", "fsd f25, 200({r})", "fsd f26, 208({r})", "fsd f27, 216({r})",
                "fsd f28, 224({r})", "fsd f29, 232({r})", "fsd f30, 240({r})", "fsd f31, 248({r})",
                w = in(reg) write.as_ptr(),
                r = in(reg) read.as_mut_ptr(),
                t = out(reg) _,
                in("a7") number,
                out("a0") _,
                out("f0") _, out("f1") _, out("f2") _, out("f3") _, out("f4") _, out("f5") _,
                out("f6") _, out("f7") _, out("f8") _, out("f9") _, out("f10") _, out("f11") _,
                out("f12") _, out("f13") _, out("f14") _, out("f15") _, out("f16") _, out("f17") _,
                out("f18") _, out("f19") _, out("f20") _, out("f21") _, out("f22") _, out("f23") _,
                out("f24") _, out("f25") _, out("f26") _, out("f27") _, out("f28") _, out("f29") _,
                out("f30") _, out("f31") _,
                options(nostack),
            );
        }
    }

    /// riscv64 fills `fcsr` with a masked control value (round toward zero,
    /// no accrued flags), so the pattern's slot 32 is replaced.
    #[cfg(fp_probe_riscv64)]
    fn normalise(write: &mut [u64; SLOTS]) {
        // `frm` = round toward zero (bits 7:5 = 001... actually RTZ is 001);
        // no accrued flags. `fcsr` is 8 bits, the rest reserved-zero.
        write[32] = 0b001 << 5;
    }

    /// Program entry point.
    fn main() -> i32 {
        let count = tairix_rt::arg_count();
        let Some(seed) = count
            .checked_sub(1)
            .and_then(tairix_rt::arg)
            .and_then(|bytes| bytes.first().copied())
        else {
            return EXIT_NO_SEED;
        };
        let seed = u64::from(seed);

        for round in 0..u64::from(rounds()) {
            let mut write = [0u64; SLOTS];
            for (index, slot) in write.iter_mut().enumerate() {
                *slot = pattern(seed, round, index);
            }
            normalise(&mut write);
            let mut read = [0u64; SLOTS];
            hold_across_trap(&write, &mut read);
            if read != write {
                return EXIT_FILE_CLOBBERED;
            }
        }
        0
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
#[cfg(not(fp_probe))]
fn main() {}
