use super::*;

fn one(model: CpuModel, bytes: &[u8]) -> Insn {
    decode(model, false, &mut SliceFetch::new(bytes))
}

#[test]
fn lengths_follow_modrm_and_immediates() {
    let m = CpuModel::I8088;
    assert_eq!(one(m, &[0x00, 0xC0]).len, 2);
    assert_eq!(one(m, &[0x01, 0x06, 0x34, 0x12]).len, 4);
    assert_eq!(one(m, &[0x81, 0x87, 0x10, 0x00, 0xCD, 0xAB]).len, 6);
    assert_eq!(one(m, &[0x83, 0x40, 0xFE, 0x05]).len, 4);
    assert_eq!(one(m, &[0xC7, 0x06, 0, 0, 0x34, 0x12]).len, 6);
    assert_eq!(one(m, &[0x9A, 1, 2, 3, 4]).len, 5);
    assert_eq!(one(m, &[0xF6, 0xC0, 0x55]).len, 3);
    assert_eq!(one(m, &[0xF6, 0xE0]).len, 2);
}

#[test]
fn bp_based_addresses_default_to_the_stack_segment() {
    let insn = one(CpuModel::I8086, &[0x8B, 0x46, 0x04]);
    let Ea::Mem(mem) = insn.ea else {
        panic!("memory operand")
    };
    assert_eq!(mem.base, Base::Bp);
    assert_eq!(mem.disp, 4);
    assert!(mem.displaced);
    assert_eq!(mem.base.default_segment(), Seg::Ss);
}

#[test]
fn the_8086_family_aliases_what_the_80186_defines() {
    assert_eq!(one(CpuModel::I8088, &[0x60, 0x10]).op, Op::Jcc(Cond::O));
    assert_eq!(one(CpuModel::I80186, &[0x60]).op, Op::Pusha);
    assert_eq!(one(CpuModel::I8088, &[0x0F]).op, Op::PopSeg(Seg::Cs));
    assert_eq!(one(CpuModel::I80186, &[0x0F]).op, Op::Invalid);
    assert_eq!(one(CpuModel::I8086, &[0xC1]).op, Op::Ret { pops: false });
    assert_eq!(
        one(CpuModel::V30, &[0xC1, 0xE0, 3]).op,
        Op::Shift(Shift::Shl, Count::Imm)
    );
    assert_eq!(one(CpuModel::I80188, &[0xF1]).op, Op::Invalid);
    assert_eq!(one(CpuModel::V20, &[0xF1]).op, Op::Prefix(Prefix::Lock));
}

#[test]
fn the_host_call_decodes_only_where_the_rom_allows() {
    let bytes = [0xFE, 0x38, 0x34, 0x12];
    let host = decode(CpuModel::I8088, true, &mut SliceFetch::new(&bytes));
    assert_eq!((host.op, host.imm, host.len), (Op::HostCall, 0x1234, 4));
    let guest = decode(CpuModel::I8088, false, &mut SliceFetch::new(&bytes));
    assert_eq!((guest.op, guest.len), (Op::PushRm, 2));
}

#[test]
fn nec_extensions_decode_their_operands() {
    let insn = one(CpuModel::V20, &[0x0F, 0x19, 0xC3, 0x0B]);
    assert_eq!(
        (insn.op, insn.w, insn.imm, insn.len),
        (Op::Nec(Nec::Test1), true, 0x0B, 4)
    );
    assert_eq!(one(CpuModel::V20, &[0x0F, 0x20]).op, Op::Nec(Nec::Add4s));
    assert_eq!(
        one(CpuModel::V20, &[0x0F, 0xFF, 0x80]).op,
        Op::Nec(Nec::Brkem)
    );
}

#[test]
fn every_byte_sequence_decodes_to_a_bounded_unit() {
    let mut state = 0x1234_5678_u32;
    for _ in 0..20_000 {
        let mut bytes = [0u8; 8];
        for b in &mut bytes {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *b = state.to_le_bytes()[0];
        }
        for model in CpuModel::ALL {
            let insn = one(model, &bytes);
            assert!(
                (1..=LONGEST).contains(&usize::from(insn.len)),
                "{bytes:02x?} on {model:?} gave {}",
                insn.len
            );
        }
    }
}
