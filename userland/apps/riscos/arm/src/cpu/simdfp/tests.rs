use super::super::tests::{processor, r, run, Ram, UNMAPPED};
use crate::cpu::{Cpu, Exception};
use crate::profile::BIT32;
use crate::psr::{self, Mode};
use crate::vfp::fpscr;

/// Where `exception` enters on the 32-bit profile.
fn vector(exception: Exception) -> u32 {
    BIT32.vectors + exception.offset()
}

/// The privileged code that grants coprocessors 10 and 11 and enables the
/// unit.
const GRANT: &[(u32, &str)] = &[
    (0xE3A0_060F, "MOV     R0,#&F00000"),
    (0xEE01_0F50, "MCR     CP15,0,R0,C1,C0,2"),
    (0xF57F_F06F, "ISB     SY"),
];
const ENABLE: &[(u32, &str)] = &[
    (0xE3A0_0101, "MOV     R0,#&40000000"),
    (0xEEE8_0A10, "VMSR    FPEXC,R0"),
];

fn f(value: f32) -> u32 {
    value.to_bits()
}

/// A processor with the unit granted and enabled by its own instructions,
/// then in `mode` at 0x8000.
fn enabled(ram: &mut Ram, mode: Mode) -> Cpu {
    ram.program(&BIT32, 0x100, GRANT);
    ram.program(&BIT32, 0x10C, ENABLE);
    let mut cpu = processor(&BIT32, Mode::Supervisor, 0x100);
    run(&mut cpu, ram, 5);
    assert_eq!(cpu.pc(), 0x114);
    cpu.set_cpsr(mode.bits()).expect("a mode the profile has");
    cpu.set_reg(crate::reg::Reg::PC, 0x8000);
    cpu
}

#[test]
fn the_unit_is_refused_until_cpacr_grants_it_and_fpexc_enables_it() {
    let mut ram = Ram::new();
    ram.program(&BIT32, 0x8000, &[(0xEE30_0A81, "VADD.F32 S0,S1,S2")]);
    let mut cpu = processor(&BIT32, Mode::Supervisor, 0x8000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), vector(Exception::Undefined), "no CPACR grant");

    ram.program(&BIT32, 0x100, GRANT);
    ram.program(
        &BIT32,
        0x10C,
        &[
            (0xEEF0_1A10, "VMRS    R1,FPSID"),
            (0xEE30_0A81, "VADD.F32 S0,S1,S2"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::Supervisor, 0x100);
    run(&mut cpu, &mut ram, 4);
    assert_eq!(cpu.reg(r(1)), 0x0003_0000, "FPSID despite FPEXC.EN clear");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), vector(Exception::Undefined), "FPEXC.EN clear");

    let mut cpu = enabled(&mut ram, Mode::User);
    ram.program(&BIT32, 0x8000, &[(0xEEF0_1A10, "VMRS    R1,FPSID")]);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(
        cpu.pc(),
        vector(Exception::Undefined),
        "FPSID is privileged"
    );

    // CPACR's privileged-only grant: Supervisor mode runs, User mode does not.
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x100,
        &[
            (0xE3A0_0605, "MOV     R0,#&500000"),
            (0xEE01_0F50, "MCR     CP15,0,R0,C1,C0,2"),
            (0xE3A0_0101, "MOV     R0,#&40000000"),
            (0xEEE8_0A10, "VMSR    FPEXC,R0"),
            (0xEE30_0A81, "VADD.F32 S0,S1,S2"),
        ],
    );
    let mut cpu = processor(&BIT32, Mode::Supervisor, 0x100);
    run(&mut cpu, &mut ram, 5);
    assert_eq!(cpu.pc(), 0x114, "privileged code reaches the unit");
    cpu.set_cpsr(Mode::User.bits()).expect("User");
    cpu.set_reg(crate::reg::Reg::PC, 0x110);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), vector(Exception::Undefined), "User code does not");

    let mut ram = Ram::new();
    let mut cpu = enabled(&mut ram, Mode::User);
    ram.program(&BIT32, 0x8000, &[(0xEE30_0A81, "VADD.F32 S0,S1,S2")]);
    cpu.vfp_mut().set_s(1, f(1.5));
    cpu.vfp_mut().set_s(2, f(2.25));
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.vfp().s(0), f(3.75));
    assert_eq!(cpu.pc(), 0x8004);
}

#[test]
fn vfp_arithmetic_rounds_as_the_fpscr_says_and_gathers_its_flags() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xEE80_0A81, "VDIV.F32 S0,S1,S2"),
            (0xEE01_0B02, "VMLA.F64 D0,D1,D2"),
        ],
    );
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.vfp_mut()
        .set_fpscr(3 << fpscr::RMODE_SHIFT | fpscr::DZC);
    cpu.vfp_mut().set_s(1, f(1.0));
    cpu.vfp_mut().set_s(2, f(3.0));
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.vfp().s(0), 0x3EAA_AAAA, "a third, toward zero");
    assert_eq!(cpu.vfp().fpscr() & fpscr::FLAGS, fpscr::DZC | fpscr::IXC);
    // Multiply then add, rounding twice: the product's 2^-54 is lost before
    // the sum, as a fused operation would keep it.
    cpu.vfp_mut().set_fpscr(0);
    let a = 1.0 + 2f64.powi(-27);
    cpu.vfp_mut().set_d(0, (-(1.0 + 2f64.powi(-26))).to_bits());
    cpu.vfp_mut().set_d(1, a.to_bits());
    cpu.vfp_mut().set_d(2, a.to_bits());
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.vfp().d(0), 0, "an exact positive zero");
}

#[test]
fn a_fixed_point_conversion_rounds_to_nearest_whatever_the_fpscr_says() {
    // 33554435 / 2 lies a half-unit above 2^24 + 1: zero would truncate it,
    // and the FPSCR here says zero.
    let mut ram = Ram::new();
    ram.program(&BIT32, 0x8000, &[(0xEEBA_0AEF, "VCVT.F32.S32 S0,S0,#1")]);
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.vfp_mut().set_fpscr(3 << fpscr::RMODE_SHIFT);
    cpu.vfp_mut().set_s(0, 0x0200_0003);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.vfp().s(0), 0x4B80_0001);
    assert_eq!(cpu.vfp().fpscr() & fpscr::FLAGS, fpscr::IXC);
}

#[test]
fn short_vectors_step_within_their_banks_and_a_first_bank_operand_stays_put() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xEE38_4A00, "VADD.F32 S8,S16,S0"),
            (0xEE3B_7A0F, "VADD.F32 S14,S22,S30"),
            (0xEE38_0A0C, "VADD.F32 S0,S16,S24"),
        ],
    );
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.vfp_mut().set_fpscr(3 << fpscr::LEN_SHIFT);
    for n in 0..32u8 {
        cpu.vfp_mut().set_s(n, f(f32::from(n)));
    }
    run(&mut cpu, &mut ram, 1);
    let lanes = [8u8, 9, 10, 11].map(|n| cpu.vfp().s(n));
    assert_eq!(
        lanes,
        [f(16.0), f(17.0), f(18.0), f(19.0)],
        "S0 added to each"
    );
    assert_eq!(cpu.vfp().s(12), f(12.0), "past the length");
    run(&mut cpu, &mut ram, 1);
    let wrapped = [14u8, 15, 8, 9].map(|n| cpu.vfp().s(n));
    assert_eq!(
        wrapped,
        [
            f(22.0 + 30.0),
            f(23.0 + 31.0),
            f(16.0 + 24.0),
            f(17.0 + 25.0)
        ],
        "each operand wraps within its own bank"
    );
    let before = cpu.vfp().s(1);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(
        cpu.vfp().s(0),
        f(16.0 + 24.0),
        "a first-bank destination is a scalar"
    );
    assert_eq!(cpu.vfp().s(1), before);

    let mut ram = Ram::new();
    ram.program(&BIT32, 0x8000, &[(0xEE38_4B0C, "VADD.F64 D4,D8,D12")]);
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.vfp_mut()
        .set_fpscr(1 << fpscr::LEN_SHIFT | 3 << fpscr::STRIDE_SHIFT);
    for n in 0..16u8 {
        cpu.vfp_mut().set_d(n, f64::from(n).to_bits());
    }
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.vfp().d(4), 20.0f64.to_bits());
    assert_eq!(cpu.vfp().d(6), 24.0f64.to_bits(), "a stride of two");
    assert_eq!(cpu.vfp().d(5), 5.0f64.to_bits());
}

#[test]
fn a_short_vector_the_architecture_leaves_unpredictable_is_refused() {
    let cases: &[(u32, &str, u32)] = &[
        (0xEE38_4A00, "VADD.F32 S8,S16,S0", 1 << fpscr::STRIDE_SHIFT),
        (0xEEA8_4A00, "VFMA.F32 S8,S16,S0", 1 << fpscr::LEN_SHIFT),
        (
            0xEE38_4B0C,
            "VADD.F64 D4,D8,D12",
            3 << fpscr::LEN_SHIFT | 3 << fpscr::STRIDE_SHIFT,
        ),
    ];
    for &(word, text, controls) in cases {
        let mut ram = Ram::new();
        ram.program(&BIT32, 0x8000, &[(word, text)]);
        let mut cpu = enabled(&mut ram, Mode::User);
        cpu.vfp_mut().set_fpscr(controls);
        run(&mut cpu, &mut ram, 1);
        assert_eq!(cpu.pc(), vector(Exception::Undefined), "{text}");
    }
    // A comparison is never a vector: it runs as a scalar whatever is set.
    let mut ram = Ram::new();
    ram.program(&BIT32, 0x8000, &[(0xEEB4_0A60, "VCMP.F32 S0,S1")]);
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.vfp_mut().set_fpscr(1 << fpscr::STRIDE_SHIFT);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), 0x8004);
}

#[test]
fn vldm_writes_its_registers_and_base_only_when_every_word_arrives() {
    let mut ram = Ram::new();
    ram.program(&BIT32, 0x8000, &[(0xECB0_0B06, "VLDMIA  R0!,{D0-D2}")]);
    for n in 0..6 {
        ram.put(0x4000 + 4 * n, 0x1111_1111 * (n + 1));
    }
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.set_reg(r(0), 0x4000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.vfp().d(0), 0x2222_2222_1111_1111, "the low word first");
    assert_eq!(cpu.vfp().d(2), 0x6666_6666_5555_5555);
    assert_eq!(cpu.reg(r(0)), 0x4018);
    // The second doubleword lies on an unmapped page.
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.set_reg(r(0), UNMAPPED - 8);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), vector(Exception::DataAbort));
    assert_eq!(cpu.vfp().d(0), 0, "nothing loaded");
    assert_eq!(cpu.reg(r(0)), UNMAPPED - 8, "nor written back");
}

#[test]
fn vldr_takes_a_double_s_words_in_the_data_endianness() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xED90_0B02, "VLDR    D0,[R0,#8]"),
            (0xF101_0200, "SETEND  BE"),
            (0xED90_0B02, "VLDR    D0,[R0,#8]"),
        ],
    );
    ram.put(0x4008, 0x0011_2233);
    ram.put(0x400C, 0x4455_6677);
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.set_reg(r(0), 0x4000);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.vfp().d(0), 0x4455_6677_0011_2233);
    run(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.vfp().d(0), 0x3322_1100_7766_5544, "the lower word high");
}

#[test]
fn structure_loads_interleave_and_fault_the_alignment_they_name() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xF420_084F, "VLD2.16 {D0-D1},[R0]"),
            (0xF421_279F, "VLD1.32 {D2},[R1:64]"),
        ],
    );
    for n in 0..4 {
        ram.put(0x4000 + 4 * n, ((2 * n + 1) << 16) | (2 * n));
    }
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.set_reg(r(0), 0x4000);
    cpu.set_reg(r(1), 0x4004);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.vfp().d(0), 0x0006_0004_0002_0000, "the even elements");
    assert_eq!(cpu.vfp().d(1), 0x0007_0005_0003_0001, "the odd");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(
        cpu.pc(),
        vector(Exception::DataAbort),
        "an address of four bytes' alignment"
    );
    assert_eq!(cpu.vfp().d(2), 0);
}

#[test]
fn vmrs_carries_a_comparison_s_flags_to_the_apsr() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xEEB4_0A60, "VCMP.F32 S0,S1"),
            (0xEEF1_FA10, "VMRS    APSR_nzcv,FPSCR"),
        ],
    );
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.vfp_mut().set_s(0, f(1.0));
    cpu.vfp_mut().set_s(1, f(2.0));
    run(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.cpsr() & psr::NZCV, psr::N, "less than");
}

#[test]
fn saturation_sets_qc_and_only_a_write_of_the_fpscr_clears_it() {
    let mut ram = Ram::new();
    ram.program(
        &BIT32,
        0x8000,
        &[
            (0xF201_0012, "VQADD.S8 D0,D1,D2"),
            (0xF201_0012, "VQADD.S8 D0,D1,D2"),
        ],
    );
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.vfp_mut().set_d(1, 0x7F7F_7F7F_7F7F_7F7F);
    cpu.vfp_mut().set_d(2, 0x0101_0101_0101_0101);
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.vfp().d(0), 0x7F7F_7F7F_7F7F_7F7F);
    assert_ne!(cpu.vfp().fpscr() & fpscr::QC, 0);
    cpu.vfp_mut().set_d(2, 0);
    run(&mut cpu, &mut ram, 1);
    assert_ne!(cpu.vfp().fpscr() & fpscr::QC, 0, "it gathers");
}

#[test]
fn t32_conditions_floating_point_by_its_block_and_refuses_armv8_s_additions_in_one() {
    let mut ram = Ram::new();
    ram.thumb(
        0x8000,
        &[
            (&[0xBF04], "ITT     EQ"),
            (&[0xEE30, 0x0A81], "VADDEQ.F32 S0,S1,S2"),
            (&[0xFE20, 0x0A81], "VSELGE.F32 S0,S1,S2"),
        ],
    );
    let mut cpu = enabled(&mut ram, Mode::User);
    cpu.set_cpsr(Mode::User.bits() | psr::T | psr::Z)
        .expect("User");
    cpu.set_reg(crate::reg::Reg::PC, 0x8000);
    cpu.vfp_mut().set_s(1, f(1.0));
    cpu.vfp_mut().set_s(2, f(2.0));
    run(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.vfp().s(0), f(3.0), "its condition held");
    run(&mut cpu, &mut ram, 1);
    assert_eq!(cpu.pc(), vector(Exception::Undefined), "VSEL in a block");
}
