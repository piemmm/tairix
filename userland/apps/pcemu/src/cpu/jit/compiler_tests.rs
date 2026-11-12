use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::*;

const AT: Key = Key {
    cs: 0x1000,
    ip: 0x0100,
};

/// `INC AX` twice, then `JMP $`.
const BLOCK: [u8; 4] = [0x40, 0x40, 0xEB, 0xFE];

fn compile(target: Target, bytes: &[u8]) -> Compile<'_> {
    Compile {
        target,
        model: CpuModel::I8088,
        key: AT,
        bytes,
    }
}

#[test]
fn a_request_and_an_answer_cross_whole() {
    for target in Target::ALL {
        for model in CpuModel::ALL {
            let sent = Compile {
                model,
                ..compile(target, &BLOCK)
            };
            let payload = sent.encode().unwrap();
            assert_eq!(Compile::decode(&payload), Ok(sent));
        }
    }
    for answer in [
        Compiled::Refused,
        Compiled::Staged {
            base: 0x7F00_0000_1000,
            len: 1,
        },
        Compiled::Staged {
            base: 0,
            len: MOST_CODE_BYTES as u64,
        },
    ] {
        assert_eq!(Compiled::decode(&answer.encode()), Ok(answer));
    }
}

#[test]
fn a_request_no_block_could_be_is_refused() {
    let good = compile(Target::X86_64, &BLOCK).encode().unwrap();
    let mut trailing = good.clone();
    trailing.push(0);
    let mut bad_target = good.clone();
    bad_target[0] = u8::try_from(Target::ALL.len()).unwrap();
    let mut bad_model = good.clone();
    bad_model[1] = 6;
    let too_many = vec![0x90; MOST_BLOCK_BYTES + 1];
    for payload in [
        trailing,
        bad_target,
        bad_model,
        compile(Target::X86_64, &[]).encode().unwrap(),
        compile(Target::X86_64, &too_many).encode().unwrap(),
        good[..good.len() - 1].to_vec(),
    ] {
        assert!(Compile::decode(&payload).is_err(), "{payload:02x?}");
    }
    let most = vec![0x90; MOST_BLOCK_BYTES];
    assert!(Compile::decode(&compile(Target::X86_64, &most).encode().unwrap()).is_ok());
}

#[test]
fn an_answer_naming_no_code_or_too_much_is_refused() {
    let staged = |len| Compiled::Staged { base: 0x1000, len }.encode();
    let mut trailing = Compiled::Refused.encode();
    trailing.push(0);
    for payload in [
        staged(0),
        staged(MOST_CODE_BYTES as u64 + 1),
        vec![2],
        vec![],
        trailing,
        staged(1)[..9].to_vec(),
    ] {
        assert!(Compiled::decode(&payload).is_err(), "{payload:02x?}");
    }
}

#[test]
fn a_block_is_compiled_for_the_target_asked_for_and_staged() {
    for target in Target::ALL {
        let payload = compile(target, &BLOCK).encode().unwrap();
        let staged = RefCell::new(Vec::new());
        let answer = answer(&payload, |code| {
            staged.borrow_mut().extend_from_slice(code);
            Some(0x5000_0000)
        });
        let block = translate(CpuModel::I8088, AT.cs, AT.ip, &compile(target, &BLOCK)).unwrap();
        let expected = tairix_codegen::compile(&block.func, target).unwrap();
        assert_eq!(
            Compiled::decode(&answer),
            Ok(Compiled::Staged {
                base: 0x5000_0000,
                len: expected.len() as u64
            }),
            "{target:?}"
        );
        assert_eq!(*staged.borrow(), expected, "{target:?}: the code staged");
    }
}

#[test]
fn nothing_is_staged_for_what_does_not_compile() {
    let staged = |payload: &[u8]| {
        let mut asked = false;
        let answer = answer(payload, |_| {
            asked = true;
            Some(0x1000)
        });
        (Compiled::decode(&answer), asked)
    };
    // `IN AL, 60h` begins no block.
    let port = compile(Target::AARCH64, &[0xE4, 0x60]).encode().unwrap();
    assert_eq!(staged(&port), (Ok(Compiled::Refused), false));
    assert_eq!(staged(&[0xFF]), (Ok(Compiled::Refused), false), "malformed");
    let unstaged = answer(&compile(Target::RISCV64, &BLOCK).encode().unwrap(), |_| {
        None
    });
    assert_eq!(
        Compiled::decode(&unstaged),
        Ok(Compiled::Refused),
        "no room"
    );
}

#[test]
fn the_heaviest_block_compiles_within_the_bound_on_every_target() {
    // Thirty-two `PUSHA`s, each behind fourteen `ES:` prefixes, on the
    // 80186: the most code any block measured compiles to.
    let mut bytes = Vec::new();
    for _ in 0..crate::cpu::jit::MAX_INSTRUCTIONS {
        bytes.extend([0x26; crate::cpu::jit::lower::MAX_PREFIXES]);
        bytes.push(0x60);
    }
    for target in Target::ALL {
        let heaviest = Compile {
            model: CpuModel::I80186,
            ..compile(target, &bytes)
        };
        let block = translate(heaviest.model, AT.cs, AT.ip, &heaviest).unwrap();
        assert_eq!(block.instructions, crate::cpu::jit::MAX_INSTRUCTIONS);
        assert!(heaviest.code().is_some(), "{target:?}: past the bound");
    }
}
