//! The module chain's tests: the built-ins at reset, guest modules' lives,
//! services, SWIs, incarnations, the RMA's reasons and the module commands.

use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_riscos_arm::psr;

use super::header::Field;
use super::inventory::{Held, INVENTORY};
use super::{CommandKind, Help};
use crate::memory::layout::{APPLICATION, RISC_OS_5, SHARED};
use crate::memory::Bits;
use crate::os::{len32, Ended, Outcome, Regs};
use crate::swi::os::{MODULE, READ_DYNAMIC_AREA};
use crate::swi::X;
use crate::testkit::asm::{self, Condition};
use crate::testkit::{with_application, Session, CODE, DATA};

/// Where a test writes a module image before it is copied into the RMA.
const IMAGE: u32 = APPLICATION + 0xA000;
/// Where a test writes the names it gives `OS_Module`.
const NAME: u32 = DATA + 0x800;
/// Where module entries record what they were given.
const SEEN: u32 = DATA;
/// A test module's SWI chunk.
const CHUNK: u32 = 0x5_A5C0;
/// `OS_Exit`'s word asking for its return code.
const ABEX: u32 = 0x5845_4241;

/// A guest module image, every offset from its start.
struct Guest {
    bytes: Vec<u8>,
}

impl Guest {
    /// A 32-bit module titled `title`, version 1.23, with no entries.
    fn new(title: &str) -> Self {
        let mut guest = Self::bare(title);
        let flags = guest.words(&[1]);
        guest.set(Field::Flags, flags);
        guest
    }

    /// A module titled `title`, version 1.23, declaring no profile.
    fn bare(title: &str) -> Self {
        let mut guest = Self {
            bytes: alloc::vec![0; 52],
        };
        let at = guest.text(title.as_bytes());
        guest.set(Field::Title, at);
        let help = alloc::format!("{title}\t1.23 (01 Jan 2020)");
        let at = guest.text(help.as_bytes());
        guest.set(Field::Help, at);
        guest
    }

    fn at(&self) -> u32 {
        len32(self.bytes.len())
    }

    fn set(&mut self, field: Field, value: u32) {
        let at = field.offset() as usize;
        self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn words(&mut self, words: &[u32]) -> u32 {
        let at = self.at();
        for word in words {
            self.bytes.extend_from_slice(&word.to_le_bytes());
        }
        at
    }

    fn text(&mut self, text: &[u8]) -> u32 {
        let at = self.at();
        self.bytes.extend_from_slice(text);
        self.bytes.push(0);
        self.bytes.resize(self.bytes.len().next_multiple_of(4), 0);
        at
    }

    /// Code built knowing its own offset, made `field`'s entry.
    fn entry(&mut self, field: Field, code: impl FnOnce(u32) -> Vec<u32>) -> u32 {
        let at = self.at();
        let words = code(at);
        self.words(&words);
        self.set(field, at);
        at
    }

    /// The standard entries: an initialisation recording R10 to R12 and
    /// counting itself, a finalisation recording R10 and R11, and SWIs
    /// answering their offset plus 100.
    fn with_entries(mut self) -> Self {
        self.entry(Field::Init, |_| record(&[(10, 0), (11, 4), (12, 8)], 12));
        self.entry(Field::Final, |_| record(&[(10, 16), (11, 20)], 24));
        let handler = self.words(&[asm::add(0, 11, 100), asm::mov_pc_lr()]);
        self.set(Field::SwiHandler, handler);
        self.set(Field::SwiChunk, CHUNK);
        let table = self.at();
        self.bytes.extend_from_slice(b"Test\0Zero\0One\0\0");
        self.bytes.resize(self.bytes.len().next_multiple_of(4), 0);
        self.set(Field::SwiTable, table);
        self
    }
}

/// Code storing each `(register, offset)` at `SEEN + offset` and counting
/// itself at `SEEN + count`, then returning.
fn record(stores: &[(u32, u16)], count: u16) -> Vec<u32> {
    let mut code = alloc::vec![asm::push(1 << 3), asm::mov(3, SEEN)];
    for &(register, offset) in stores {
        code.push(asm::str(register, 3, offset));
    }
    code.extend([
        asm::push(1 << 0),
        asm::ldr(0, 3, count),
        asm::add(0, 0, 1),
        asm::str(0, 3, count),
        asm::pull(1 << 0),
        asm::pull(1 << 3),
        asm::mov_pc_lr(),
    ]);
    code
}

/// Code returning the error `number` and `text`, held after it.
fn refusal(at: u32, number: u32, text: &[u8]) -> Vec<u32> {
    let mut code = alloc::vec![
        asm::adr(0, at, at + 12),
        asm::set_flags(psr::V),
        asm::mov_pc_lr(),
        number,
    ];
    let mut block = text.to_vec();
    block.push(0);
    block.resize(block.len().next_multiple_of(4), 0);
    code.extend(
        block
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w)),
    );
    code
}

fn session() -> Session {
    with_application(&SHARED, Bits::Bit32)
}

/// `OS_Module` from User mode, without X: its registers, or its error's
/// number.
fn module(s: &mut Session, registers: [u32; 13]) -> Result<Regs, u32> {
    let (regs, _) = s.user_swi(X | MODULE, registers);
    if regs.flag(psr::V) {
        Err(s.peek(regs.r[0]))
    } else {
        Ok(regs)
    }
}

/// `OS_Module` `reason` with R1 the string `name`.
fn named(s: &mut Session, reason: u32, name: &[u8]) -> Result<Regs, u32> {
    let mut text = name.to_vec();
    text.push(b'\r');
    s.write(NAME, &text);
    module(s, [reason, NAME, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
}

/// Copies `guest` into the RMA and starts it, as `OS_Module` 11 does.
fn copy_in(s: &mut Session, guest: &Guest) -> Result<Regs, u32> {
    s.write(IMAGE, &guest.bytes);
    module(s, [11, IMAGE, guest.at(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
}

/// The text of the error SWI `number` gives with `registers`.
fn error_text(s: &mut Session, number: u32, registers: [u32; 13]) -> Vec<u8> {
    let (regs, _) = s.user_swi(X | number, registers);
    assert!(regs.flag(psr::V), "SWI &{number:X} refused");
    s.error_text(regs.r[0])
}

/// The string at `at`, to its first byte at or below a space.
fn string(s: &mut Session, at: u32) -> Vec<u8> {
    let (text, _) = s
        .kernel
        .os()
        .read_until(at, 256, |b| b < b' ')
        .expect("readable");
    text
}

/// The ROM modules the session starts with.
fn rom_count(s: &mut Session) -> u32 {
    len32(s.kernel.os().world.modules.roms.len())
}

/// Their titles, in the ROM's order.
fn rom_titles(s: &mut Session) -> alloc::vec::Vec<&'static str> {
    s.kernel
        .os()
        .world
        .modules
        .roms
        .iter()
        .map(|rom| rom.builtin.title)
        .collect()
}

#[test]
fn the_kernel_is_first_in_the_chain_its_image_in_the_host_pages() {
    let mut s = session();
    let found = named(&mut s, 18, b"utilitymodule").expect("found");
    assert_eq!((found.r[1], found.r[2]), (0, 0));
    let code = found.r[3];
    assert!(SHARED.host.contains(&code));
    assert_eq!(string(&mut s, found.r[5]), b"Base");
    let title = s.peek(code + 16);
    assert_eq!(string(&mut s, code + title), b"UtilityModule");
    let help = s.peek(code + 20);
    let (help, _) = s
        .kernel
        .os()
        .read_until(code + help, 64, |b| b == 0)
        .expect("readable");
    assert_eq!(
        help, b"UtilityModule\t5.30 (24 Apr 2024)",
        "its version, and the release's date"
    );
    assert_eq!(s.peek(code + 28), 0xF0_0000);
    let flags = s.peek(code + 48);
    assert_eq!(s.peek(code + flags) & 1, 1);
    let table = s.peek(code + 36);
    assert_eq!(s.read(code + table, 17), b"ARM\0IMB\0IMBRange\0");
}

#[test]
fn a_built_in_s_help_string_names_it_in_fifteen_characters_at_most() {
    let mut s = session();
    for (title, help) in [
        (&b"TerritoryManager"[..], &b"Territories\t"[..]),
        (b"InternationalKeyboard", b"Key Layouts\t"),
        (b"MessageTrans", b"MessageTrans\t"),
        (b"UtilityModule", b"UtilityModule\t"),
    ] {
        let code = named(&mut s, 18, title).expect("found").r[3];
        let at = code + s.peek(code + 20);
        let (text, _) = s
            .kernel
            .os()
            .read_until(at, 64, |b| b == 0)
            .expect("readable");
        assert!(
            text.starts_with(help),
            "{}",
            alloc::string::String::from_utf8_lossy(&text)
        );
    }
}

#[test]
fn every_command_of_the_kernel_is_in_its_table_with_its_help_and_syntax() {
    let mut s = session();
    let code = named(&mut s, 18, b"UtilityModule").expect("found").r[3];
    let mut at = code + s.peek(code + 24);
    for command in super::utility::UTILITY.commands {
        let name = string(&mut s, at);
        assert_eq!(name, command.name.as_bytes());
        at = (at + len32(name.len()) + 1 + 3) & !3;
        assert_eq!(
            s.peek(at) != 0,
            command.run.is_some(),
            "{} has code if it runs",
            command.name
        );
        let help_is_code = match command.help {
            Help::Code(_) => crate::cli::table::HELP_IS_CODE,
            Help::Text(_) => 0,
        };
        let kind = match command.kind {
            CommandKind::Command => 0,
            CommandKind::Filing => crate::cli::table::FILING,
            CommandKind::Configure => crate::cli::table::CONFIGURE,
        };
        let info = s.peek(at + 4);
        assert_eq!(
            info,
            u32::from(command.min) | u32::from(command.max) << 16 | help_is_code | kind
        );
        let (syntax, help) = (s.peek(at + 8), s.peek(at + 12));
        assert_ne!(help, 0, "{} has help", command.name);
        if let Help::Text(text) = command.help {
            let (read, _) = s
                .kernel
                .os()
                .read_until(code + help, 512, |b| b == 0)
                .expect("its help");
            let expected = match command.syntax {
                Some(syntax) => [text.as_bytes(), b"\r", syntax.as_bytes()].concat(),
                None => text.as_bytes().to_vec(),
            };
            assert_eq!(read, expected, "{}'s help ends in its syntax", command.name);
        }
        match command.syntax {
            Some(text) => assert_eq!(string(&mut s, code + syntax), text.as_bytes()),
            None => assert_eq!(syntax, 0, "{} has no syntax", command.name),
        }
        at += 16;
    }
    assert_eq!(s.read(at, 1), [0], "the table ends");
}

#[test]
fn rom_modules_list_the_built_ins_and_nothing_past_them() {
    let mut s = session();
    let first = module(&mut s, [20, 0, u32::MAX, 0, 0, 0x55, 0, 0, 0, 0, 0, 0, 0]).expect("one");
    assert_eq!((first.r[1], first.r[2], first.r[4]), (1, u32::MAX, 1));
    assert_eq!(string(&mut s, first.r[3]), b"UtilityModule");
    assert_eq!(first.r[5], 0x55, "a main ROM module's chunk is not given");
    assert_eq!(first.r[6], 0x0005_3000);
    let roms = rom_count(&mut s);
    for place in 1..roms {
        let rom = module(&mut s, [20, place, u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .expect("a built-in");
        assert_eq!(rom.r[1], place + 1, "each in turn");
    }
    assert_eq!(
        module(&mut s, [19, roms, u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(0x108)
    );
    assert_eq!(
        module(&mut s, [19, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(0x108)
    );
    let end = module(&mut s, [21, u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("the end");
    assert_eq!(s.read(end.r[2] - 4, 5), [0, 0, 0, 0, b'\r']);
    assert_eq!(
        module(&mut s, [21, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(0x1EA)
    );
}

#[test]
fn the_kernel_refuses_to_die_or_to_be_replaced() {
    let mut s = session();
    assert_eq!(named(&mut s, 4, b"UtilityModule"), Err(0x104));
    assert!(named(&mut s, 18, b"UtilityModule").is_ok());
    let pretender = Guest::new("UtilityModule");
    s.write(IMAGE, &pretender.bytes);
    let text = error_text(
        &mut s,
        MODULE,
        [11, IMAGE, pretender.at(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(text, b"Module UtilityModule refuses to be replaced");
    assert_eq!(
        module(&mut s, [9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).map(|_| ()),
        Ok(())
    );
    assert!(
        named(&mut s, 18, b"UtilityModule").is_ok(),
        "*RMClear leaves the ROM"
    );
}

#[test]
fn the_instruction_barriers_answer_and_are_named() {
    let mut s = session();
    let (barrier, _) = s.user_swi(
        X | 0xF0_0001,
        [0x8000, 0x9000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(!barrier.flag(psr::V));
    let text = error_text(&mut s, 0xF0_0002, [0; 13]);
    assert_eq!(text, b"Module UtilityModule provides no such SWI");
    let (named, _) = s.user_swi(
        X | 0x38,
        [0xF0_0001, NAME, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(!named.flag(psr::V));
    assert_eq!(string(&mut s, NAME), b"ARM_IMBRange");
    s.write(NAME, b"XARM_IMB\0");
    let (number, _) = s.user_swi(X | 0x39, [0, NAME, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(number.r[0], X | 0xF0_0000);
}

#[test]
fn a_guest_module_lives_and_dies_by_its_entries() {
    let mut s = session();
    let guest = Guest::new("TestMod").with_entries();
    copy_in(&mut s, &guest).expect("linked");
    assert_eq!(s.peek(SEEN + 12), 1, "initialised once");
    assert_eq!(
        s.peek(SEEN + 4),
        0,
        "R11 the hardware base of a first incarnation"
    );
    let found = named(&mut s, 18, b"TestMod").expect("found");
    assert_eq!(found.r[1], rom_count(&mut s), "after the ROM's");
    assert!(SHARED.rma.contains(&found.r[3]));
    assert_eq!(s.peek(SEEN + 8), found.r[5] - 4, "R12 its private word");
    assert_eq!(string(&mut s, found.r[5]), b"Base");
    let (swi, _) = s.user_swi(X | CHUNK | 1, [0; 13]);
    assert_eq!(swi.r[0], 101);
    let (name, _) = s.user_swi(
        X | 0x38,
        [X | CHUNK | 1, NAME, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(!name.flag(psr::V));
    assert_eq!(string(&mut s, NAME), b"XTest_One");
    s.write(NAME, b"Test_Zero\0");
    let (number, _) = s.user_swi(X | 0x39, [0, NAME, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(number.r[0], CHUNK);
    named(&mut s, 4, b"testmod").expect("killed");
    assert_eq!(
        (s.peek(SEEN + 16), s.peek(SEEN + 24)),
        (1, 1),
        "finalised fatally, once"
    );
    assert_eq!(named(&mut s, 18, b"TestMod"), Err(0x102));
    s.write(NAME, b"TestMod 1.00\r");
    let text = error_text(&mut s, MODULE, [18, NAME, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        text, b"Module 'TestMod 1.00' not found",
        "the rest of the string is named"
    );
    let (gone, _) = s.user_swi(X | CHUNK, [0; 13]);
    assert!(gone.flag(psr::V), "its SWIs go with it");
}

/// The RMA's bytes in use: its size less the free space `OS_Module` 5
/// counts, so a claim that grew it is still seen given back.
fn rma_in_use(s: &mut Session) -> u32 {
    let (area, _) = s.user_swi(
        X | READ_DYNAMIC_AREA,
        [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    let described = module(s, [5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("described");
    area.r[1] - described.r[3]
}

#[test]
fn an_initialisation_error_is_kept_and_leaves_nothing_behind() {
    let mut s = session();
    let before = rma_in_use(&mut s);
    let mut guest = Guest::new("Refuser");
    guest.entry(Field::Init, |at| refusal(at, 0x1234, b"Init refused"));
    s.write(IMAGE, &guest.bytes);
    let (regs, _) = s.user_swi(
        X | MODULE,
        [11, IMAGE, guest.at(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(regs.flag(psr::V));
    assert!(
        !SHARED.rma.contains(&regs.r[0]),
        "the block is the kernel's copy"
    );
    assert_eq!(s.peek(regs.r[0]), 0x1234);
    assert_eq!(s.error_text(regs.r[0]), b"Init refused");
    assert_eq!(named(&mut s, 18, b"Refuser"), Err(0x102));
    assert_eq!(rma_in_use(&mut s), before, "the RMA as it was");
}

#[test]
fn a_module_that_will_not_die_runs_on_as_it_was() {
    let mut s = session();
    let mut guest = Guest::new("Stubborn").with_entries();
    guest.entry(Field::Final, |at| refusal(at, 0x4321, b"Not yet"));
    copy_in(&mut s, &guest).expect("linked");
    assert_eq!(named(&mut s, 4, b"Stubborn"), Err(0x4321));
    let found = named(&mut s, 18, b"Stubborn").expect("still there");
    assert_eq!(found.r[2], 0);
    let (swi, _) = s.user_swi(X | CHUNK, [0; 13]);
    assert_eq!(swi.r[0], 100);
}

#[test]
fn the_current_active_object_keeps_its_module() {
    let mut s = session();
    copy_in(&mut s, &Guest::new("Busy").with_entries()).expect("linked");
    let code = named(&mut s, 18, b"Busy").expect("found").r[3];
    s.kernel.os().world.env.set_active_object(code + 8);
    assert_eq!(named(&mut s, 4, b"Busy"), Err(0x104));
    s.kernel.os().world.env.set_active_object(0);
    assert!(named(&mut s, 4, b"Busy").is_ok());
}

#[test]
fn incarnations_are_made_named_preferred_and_killed_one_by_one() {
    let mut s = session();
    copy_in(&mut s, &Guest::new("Multi").with_entries()).expect("linked");
    named(&mut s, 14, b"Multi%Two params").expect("a second");
    assert_eq!(s.peek(SEEN + 4), 1, "R11 the incarnations it had");
    let env = s.peek(SEEN);
    assert_eq!(string(&mut s, env), b"params");
    let roms = rom_count(&mut s);
    let first = module(&mut s, [12, roms, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("one");
    assert_eq!(
        string(&mut s, first.r[5]),
        b"Two",
        "the newest is preferred"
    );
    assert_eq!((first.r[1], first.r[2]), (roms, 1));
    let second = module(&mut s, [12, roms, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("two");
    assert_eq!(string(&mut s, second.r[5]), b"Base");
    assert_eq!((second.r[1], second.r[2]), (roms + 1, 0));
    assert_eq!(named(&mut s, 14, b"Multi%two"), Err(0x10B));
    assert_eq!(named(&mut s, 14, b"Multi%"), Err(0x10A));
    assert_eq!(named(&mut s, 14, b"Multi"), Err(0x10A));
    s.write(NAME + 0x40, b"Uno\r");
    s.write(NAME, b"Multi%Base\r");
    module(
        &mut s,
        [15, NAME, NAME + 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("renamed");
    let renamed = named(&mut s, 18, b"Multi%Uno").expect("by its new name");
    assert_eq!(renamed.r[2], 1);
    named(&mut s, 16, b"Multi%Uno").expect("preferred");
    assert_eq!(named(&mut s, 18, b"Multi").expect("preferred").r[2], 0);
    named(&mut s, 4, b"Multi%Two").expect("one killed");
    assert_eq!(s.peek(SEEN + 20), 1, "R11 its place");
    assert_eq!(named(&mut s, 18, b"Multi%Two"), Err(0x10C));
    assert!(named(&mut s, 18, b"Multi%Uno").is_ok(), "the other runs on");
}

#[test]
fn a_26_bit_module_runs_in_its_profile_where_26_bit_code_may() {
    let mut s = with_application(&SHARED, Bits::Bit26);
    let mut guest = Guest::bare("Old");
    guest.entry(Field::Init, |_| record(&[(10, 0)], 12));
    let handler = guest.words(&[asm::add(0, 11, 7), asm::mov_pc_lr()]);
    guest.set(Field::SwiHandler, handler);
    guest.set(Field::SwiChunk, CHUNK);
    copy_in(&mut s, &guest).expect("linked");
    assert_eq!(s.peek(SEEN + 12), 1);
    let roms = rom_count(&mut s) as usize;
    let key = s.kernel.os().world.modules.nth(roms).expect("linked");
    assert_eq!(
        s.kernel.os().world.modules.get(key).map(|m| m.shape.bits),
        Some(Bits::Bit26)
    );
    let (swi, _) = s.user_swi(X | CHUNK | 2, [0; 13]);
    assert_eq!(swi.r[0], 9);
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    let text = {
        let guest = Guest::bare("Old");
        s.write(IMAGE, &guest.bytes);
        error_text(
            &mut s,
            MODULE,
            [11, IMAGE, guest.at(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )
    };
    assert_eq!(text, b"Module 'Old' cannot run in 32-bit mode");
}

#[test]
fn a_header_is_checked_before_anything_of_it_runs() {
    let mut s = session();
    let mut far = Guest::new("Far").with_entries();
    far.set(Field::Title, 0x10_0000);
    s.write(IMAGE, &far.bytes);
    let text = error_text(
        &mut s,
        MODULE,
        [11, IMAGE, far.at(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(text, b"A module header field is out of range");
    let mut crooked = Guest::new("Crooked").with_entries();
    crooked.set(Field::Service, 6);
    assert_eq!(copy_in(&mut s, &crooked), Err(0x10E));
    let mut chunked = Guest::new("Chunked").with_entries();
    chunked.set(Field::SwiChunk, CHUNK | 1);
    s.write(IMAGE, &chunked.bytes);
    let text = error_text(
        &mut s,
        MODULE,
        [11, IMAGE, chunked.at(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(text, b"Module 'Chunked' cannot run in 32-bit mode");
    assert_eq!(s.peek(SEEN + 12), 0, "nothing ran");
    let mut percent = Guest::new("Per%Cent");
    percent.set(Field::Init, 0);
    assert!(copy_in(&mut s, &percent).is_ok(), "a title may hold a %");
}

#[test]
fn a_squeezed_module_is_expanded_as_it_starts() {
    let mut s = session();
    let plain = Guest::new("Squash").with_entries();
    let squeezed = super::squeeze::tests::squeeze(&plain.bytes);
    s.write(IMAGE, &squeezed);
    let size = len32(squeezed.len());
    module(&mut s, [11, IMAGE, size, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("expanded and linked");
    assert_eq!(s.peek(SEEN + 12), 1);
    let code = named(&mut s, 18, b"Squash").expect("found").r[3];
    assert_eq!(
        s.read(code, plain.bytes.len()),
        plain.bytes,
        "the module as it was squeezed"
    );
    let (swi, _) = s.user_swi(X | CHUNK | 3, [0; 13]);
    assert_eq!(swi.r[0], 103);
}

/// A module whose service handler records, at `SEEN + slot`, the order it
/// was called in and the R1 it was given, claiming the call when R1 is
/// `claim`.
fn listener(title: &str, slot: u16, claim: u32) -> Guest {
    let mut guest = Guest::new(title);
    guest.entry(Field::Service, |_| {
        alloc::vec![
            asm::push(1 << 3 | 1 << 4),
            asm::mov(3, SEEN),
            asm::ldr(4, 3, 0x40),
            asm::add(4, 4, 1),
            asm::str(4, 3, 0x40),
            asm::str(4, 3, slot),
            asm::str(1, 3, slot + 4),
            asm::cmp_imm(1, claim),
            asm::when(Condition::Eq, asm::mov(1, 0)),
            asm::pull(1 << 3 | 1 << 4),
            asm::mov_pc_lr(),
        ]
    });
    guest
}

/// Links a module counting, at `at`, the times service `number` comes, and
/// keeping the R2 and R3 it last came with four and eight bytes on.
pub(crate) fn watch_service(s: &mut Session, number: u32, at: u32) {
    let mut guest = Guest::new(&alloc::format!("Watch{number:X}"));
    guest.entry(Field::Service, |_| {
        let matched = |instruction| asm::when(Condition::Eq, instruction);
        alloc::vec![
            asm::push(1 << 4 | 1 << 5),
            asm::ldr(4, 15, 32),
            asm::cmp(1, 4),
            matched(asm::mov(5, at)),
            matched(asm::ldr(4, 5, 0)),
            matched(asm::add(4, 4, 1)),
            matched(asm::str(4, 5, 0)),
            matched(asm::str(2, 5, 4)),
            matched(asm::str(3, 5, 8)),
            asm::pull(1 << 4 | 1 << 5),
            asm::mov_pc_lr(),
            number,
        ]
    });
    s.poke(at, &[0, 0, 0]);
    copy_in(s, &guest).expect("linked");
}

#[test]
fn a_service_goes_round_the_modules_in_their_order_until_one_claims_it() {
    let mut s = session();
    copy_in(&mut s, &listener("Early", 0x48, 0)).expect("linked");
    copy_in(&mut s, &listener("Late", 0x50, 0x5B)).expect("linked");
    copy_in(&mut s, &listener("Last", 0x58, 0)).expect("linked");
    s.poke(SEEN + 0x40, &[0]);
    let (regs, _) = s.user_swi(0x30, [0x77, 0x5A, 0x99, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!((s.peek(SEEN + 0x48), s.peek(SEEN + 0x4C)), (1, 0x5A));
    assert_eq!((s.peek(SEEN + 0x50), s.peek(SEEN + 0x54)), (2, 0x5A));
    assert_eq!((s.peek(SEEN + 0x58), s.peek(SEEN + 0x5C)), (3, 0x5A));
    assert_eq!(regs.r[1], 0x5A, "unclaimed");
    s.poke(SEEN + 0x40, &[0]);
    s.poke(SEEN + 0x58, &[0]);
    let (regs, _) = s.user_swi(0x30, [0, 0x5B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(regs.r[1], 0, "claimed");
    assert_eq!(s.peek(SEEN + 0x58), 0, "no one after the claim");
    let replacement = listener("EARLY", 0x60, 0);
    let block = module(
        &mut s,
        [6, 0, 0, replacement.at(), 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("claimed")
    .r[2];
    s.write(block, &replacement.bytes);
    module(&mut s, [10, block, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("replaced");
    s.poke(SEEN + 0x40, &[0]);
    s.user_swi(0x30, [0, 0x5C, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        s.peek(SEEN + 0x60),
        1,
        "the replacement in its namesake's place"
    );
}

#[test]
fn a_replacement_takes_its_namesake_s_place() {
    let mut s = session();
    copy_in(&mut s, &Guest::new("First").with_entries()).expect("linked");
    copy_in(&mut s, &Guest::new("Second")).expect("linked");
    let replacement = Guest::new("FIRST");
    s.write(IMAGE, &replacement.bytes);
    let mut block = module(
        &mut s,
        [6, 0, 0, replacement.at(), 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("claimed");
    s.write(block.r[2], &replacement.bytes);
    block.r[1] = block.r[2];
    module(&mut s, [10, block.r[1], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("added");
    assert_eq!(s.peek(SEEN + 24), 1, "the namesake finalised");
    let found = named(&mut s, 18, b"First").expect("found");
    assert_eq!(found.r[1], rom_count(&mut s), "in its place, before Second");
    assert_eq!(found.r[3], block.r[1]);
}

#[test]
fn the_rma_reasons_claim_extend_and_free() {
    let mut s = session();
    let claimed = module(&mut s, [6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("claimed");
    assert!(SHARED.rma.contains(&claimed.r[2]));
    assert_eq!(s.peek(claimed.r[2] - 4), 32, "even nothing has a block");
    let block = module(&mut s, [6, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect("claimed")
        .r[2];
    s.poke(block, &[0x1111_1111, 0x2222_2222]);
    let grown = module(&mut s, [13, 0, block, 200, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("grown");
    assert_eq!(grown.r[3], 200, "the change as it was given");
    assert_eq!(
        (s.peek(grown.r[2]), s.peek(grown.r[2] + 4)),
        (0x1111_1111, 0x2222_2222)
    );
    assert!(s.peek(grown.r[2] - 4) >= 300);
    module(&mut s, [7, 0, grown.r[2], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("freed");
    assert_eq!(
        module(&mut s, [7, 0, grown.r[2], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(0x185)
    );
    let aligned = module(&mut s, [24, 0, 0, 10, 4096, 0, 0, 0, 0, 0, 0, 0, 0]).expect("aligned");
    assert_eq!(aligned.r[2] % 4096, 0);
    assert_eq!(aligned.r[4], 4096);
    assert_eq!(
        module(&mut s, [24, 0, 0, 10, 48, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(0x117)
    );
    assert_eq!(
        module(&mut s, [24, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(0x117)
    );
    let before = module(&mut s, [5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("described");
    assert!(before.r[2] <= before.r[3]);
    assert!(module(&mut s, [8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_ok());
}

#[test]
fn rm_clear_kills_all_but_the_rom_and_the_invincible() {
    let mut s = session();
    copy_in(&mut s, &Guest::new("Gone").with_entries()).expect("linked");
    let mut stays = Guest::new("Stays").with_entries();
    let final_entry = u32::from_le_bytes(stays.bytes[8..12].try_into().expect("a word"));
    stays.set(Field::Final, final_entry | 0x8000_0000);
    copy_in(&mut s, &stays).expect("linked");
    module(&mut s, [9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("cleared");
    assert_eq!(named(&mut s, 18, b"Gone"), Err(0x102));
    assert!(named(&mut s, 18, b"Stays").is_ok());
    assert!(named(&mut s, 18, b"UtilityModule").is_ok());
}

#[test]
fn enumeration_reports_a_module_between_incarnations() {
    let mut s = session();
    copy_in(&mut s, &Guest::new("Between").with_entries()).expect("linked");
    let roms = rom_count(&mut s) as usize;
    let key = s.kernel.os().world.modules.nth(roms).expect("linked");
    let incarnation = s
        .kernel
        .os()
        .world
        .modules
        .detach(key, 0)
        .expect("detached");
    let at = len32(roms);
    let between = module(&mut s, [12, at, 0, 0, 0, 0x77, 0, 0, 0, 0, 0, 0, 0]).expect("enumerated");
    assert_eq!(
        (between.r[1], between.r[2], between.r[4], between.r[5]),
        (at + 1, 0, 0xDEAD_DEAD, 0x77)
    );
    let pwp = module(&mut s, [22, at, 0, 0, 0, 0x77, 0, 0, 0, 0, 0, 0, 0]).expect("enumerated");
    assert_eq!(pwp.r[4], 0);
    assert_eq!(
        module(&mut s, [12, at, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(0x109)
    );
    assert_eq!(
        module(&mut s, [12, at + 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(0x108)
    );
    assert!(s.kernel.os().world.modules.attach(key, 0, incarnation));
}

#[test]
fn unknown_reasons_are_refused() {
    let mut s = session();
    for reason in [23, 25, u32::MAX] {
        assert_eq!(
            module(&mut s, [reason, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            Err(0x105)
        );
    }
    let text = error_text(&mut s, MODULE, [23, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(text, b"Reason code not known to OS_Module");
}

#[test]
fn exit_and_die_kills_the_module_and_exits() {
    let mut s = session();
    copy_in(&mut s, &Guest::new("Dying").with_entries()).expect("linked");
    let code = named(&mut s, 18, b"Dying").expect("found").r[3];
    s.kernel.os().world.env.set_active_object(code);
    s.write(NAME, b"Dying\r");
    let outcome = s.start_swi(0x50, [0, ABEX, 7, NAME, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(outcome, Outcome::Ended(Ended::Exit { code: 7 }));
    assert_eq!(s.peek(SEEN + 24), 1, "finalised despite being active");
}

#[test]
fn entering_a_module_starts_it_as_the_application() {
    let mut s = session();
    let mut guest = Guest::new("Starter").with_entries();
    guest.entry(Field::Start, |_| {
        alloc::vec![
            asm::mov(1, SEEN),
            asm::str(0, 1, 0x60),
            asm::str(12, 1, 0x64),
            asm::mov(1, 0),
            asm::swi(0x11),
        ]
    });
    copy_in(&mut s, &guest).expect("linked");
    let found = named(&mut s, 18, b"Starter").expect("found");
    s.write(NAME, b"Starter\r");
    s.write(NAME + 0x40, b"its tail\r");
    let outcome = s.start_swi(MODULE, [2, NAME, NAME + 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(outcome, Outcome::Ended(Ended::Exit { code: 0 }));
    let line = s.peek(SEEN + 0x60);
    assert_eq!(
        s.read(line, 17),
        b"Starter its tail\0",
        "its title, then the tail"
    );
    assert_eq!(s.peek(SEEN + 0x64), found.r[5] - 4);
    copy_in(&mut s, &Guest::new("Startless")).expect("linked");
    assert!(
        named(&mut s, 2, b"Startless").is_ok(),
        "nothing to start returns"
    );
}

#[test]
fn loading_a_module_file_reads_it_through_os_file() {
    let mut s = session();
    let guest = Guest::new("Loaded").with_entries();
    s.write(IMAGE, &guest.bytes);
    let length = guest.at();
    // A FileV claimant serving one file: OS_File 5 reads its catalogue
    // entry, OS_File 255 copies it to R2.
    let claimant = CODE;
    let load_address = claimant + 64;
    s.poke(
        claimant,
        &[
            asm::cmp_imm(0, 5),
            asm::when(Condition::Eq, asm::b(claimant + 4, claimant + 20)),
            asm::cmp_imm(0, 255),
            asm::when(Condition::Eq, asm::b(claimant + 12, claimant + 36)),
            asm::mov_pc_lr(),
            asm::mov(0, 1),
            asm::ldr(2, 15, 32),
            asm::ldr(4, 15, 32),
            asm::claim(),
            asm::mov(5, IMAGE),
            asm::ldr(6, 15, 20),
            asm::ldr_post(3, 5, 4),
            asm::str_post(3, 2, 4),
            asm::subs(6, 6, 4),
            asm::when(Condition::Ne, asm::b(claimant + 56, claimant + 44)),
            asm::claim(),
            0xFFFF_FA00,
            length,
        ],
    );
    let (claimed, _) = s.user_swi(X | 0x1F, [8, claimant, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(!claimed.flag(psr::V));
    named(&mut s, 1, b"Loaded.file  some parameters").expect("loaded");
    let env = s.peek(SEEN);
    assert_eq!(string(&mut s, env), b"some parameters");
    let code = named(&mut s, 18, b"Loaded").expect("found").r[3];
    assert_eq!(s.read(code, guest.bytes.len()), guest.bytes);
    s.poke(load_address, &[0xFFFF_F000]);
    assert_eq!(
        named(&mut s, 1, b"Other"),
        Err(0x100),
        "not a module's file type"
    );
}

/// A module whose every service calls `OS_Exit` when it is `service`.
fn exiter(service: u32) -> Guest {
    let mut guest = Guest::new("Exiter");
    guest.entry(Field::Service, |_| {
        alloc::vec![
            asm::cmp_imm(1, service),
            asm::when(Condition::Ne, asm::mov_pc_lr()),
            asm::mov(1, 0),
            asm::swi(0x11),
        ]
    });
    guest
}

#[test]
fn an_unwind_while_the_rma_grows_ends_the_call_rather_than_filling_it() {
    let mut s = session();
    copy_in(&mut s, &exiter(0x4E)).expect("linked");
    let outcome = s.start_swi(MODULE, [6, 0, 0, 0x40_0000, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(outcome, Outcome::Ended(Ended::Exit { code: 0 }));
}

#[test]
fn an_unwind_while_the_system_heap_grows_ends_setting_a_variable() {
    let mut s = session();
    s.application(Bits::Bit32, 0x4_0000);
    copy_in(&mut s, &exiter(0x4E)).expect("linked");
    s.write(NAME, b"Big\0");
    let value = alloc::vec![b'x'; 0x8000];
    s.write(APPLICATION + 0x1_0000, &value);
    let start = [
        NAME,
        APPLICATION + 0x1_0000,
        0x8000,
        0,
        4,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    let outcome = s.start_swi(0x24, start);
    assert_eq!(outcome, Outcome::Ended(Ended::Exit { code: 0 }));
    s.application(Bits::Bit32, 0x1_0000);
    s.write(NAME, b"Big\0");
    let (read, _) = s.user_swi(X | 0x23, [NAME, IMAGE, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(read.flag(psr::V), "the variable was never made");
}

#[test]
fn modules_lists_each_incarnation_of_each_module() {
    let mut s = session();
    copy_in(&mut s, &Guest::new("Lister").with_entries()).expect("linked");
    named(&mut s, 14, b"Lister%Two").expect("a second");
    s.capture_output();
    let code = named(&mut s, 18, b"Lister").expect("found").r[3];
    super::commands::modules_command(&mut s.kernel.os(), &mut Regs::default()).expect("listed");
    let text = s.output();
    let private = |s: &mut Session, name: &[u8]| {
        let regs = named(s, 18, name).expect("found");
        s.peek(regs.r[5] - 4)
    };
    let hex = |n: u32| alloc::format!("{n:08X}");
    let mut expected = alloc::string::String::from("No. Position Workspace Name\n\r");
    for (n, rom) in rom_titles(&mut s).into_iter().enumerate() {
        let at = named(&mut s, 18, rom.as_bytes()).expect("found").r[3];
        let workspace = private(&mut s, rom.as_bytes());
        let _ = write!(
            expected,
            "{:3} {} {}  {rom}\n\r",
            n + 1,
            hex(at),
            hex(workspace)
        );
    }
    let two = private(&mut s, b"Lister%Two");
    let base = private(&mut s, b"Lister%Base");
    let _ = write!(
        expected,
        "{:3} {} {}  Lister%Two\n\r    {} {}  Lister%Base\n\r",
        rom_count(&mut s) + 1,
        hex(code),
        hex(two),
        hex(code),
        hex(base),
    );
    assert_eq!(alloc::string::String::from_utf8_lossy(&text), expected);
}

#[test]
fn rom_modules_lists_the_built_ins_and_unplug_stops_them() {
    let mut s = session();
    s.capture_output();
    super::commands::rom_modules_command(&mut s.kernel.os(), &mut Regs::default()).expect("listed");
    let text = s.output();
    let mut expected =
        alloc::string::String::from("No. Position    Module name             Version Status\n\r");
    for (n, title) in rom_titles(&mut s).into_iter().enumerate() {
        let row = super::inventory::INVENTORY
            .iter()
            .find(|rom| rom.title == title)
            .expect("in the inventory");
        let version = alloc::format!("{}.{:02}", row.version / 100, row.version % 100);
        let _ = write!(
            expected,
            "{:3} System ROM  {title:<24}{version:<8}Active\n\r",
            n + 1
        );
    }
    assert_eq!(alloc::string::String::from_utf8_lossy(&text), expected);
    super::commands::unplug(&mut s.kernel.os(), &mut Regs::default()).expect("listed");
    assert_eq!(s.output(), b"No module is unplugged\n\r");
}

#[test]
fn rm_ensure_compares_versions_and_names_what_is_missing() {
    let mut s = session();
    copy_in(&mut s, &Guest::new("Versioned")).expect("linked");
    let ensure = |s: &mut Session, tail: &[u8]| {
        let mut text = tail.to_vec();
        text.push(b'\r');
        s.write(NAME, &text);
        let count = tail.split(|&b| b == b' ').filter(|w| !w.is_empty()).count();
        let mut regs = Regs::new([NAME, len32(count), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        match super::commands::rm_ensure(&mut s.kernel.os(), &mut regs) {
            Ok(()) => None,
            Err(crate::os::Fail::Error(block)) => Some(s.error_text(block)),
            Err(unwind) => panic!("{unwind:?}"),
        }
    };
    assert_eq!(ensure(&mut s, b"Versioned 1.23"), None);
    assert_eq!(ensure(&mut s, b"Versioned 1.2"), None, "1.23 is past 1.20");
    assert_eq!(
        ensure(&mut s, b"versioned 1.3"),
        Some(b"Module Versioned is an older version than required".to_vec())
    );
    assert_eq!(
        ensure(&mut s, b"Absent 1.00"),
        Some(b"Module 'Absent 1.00' not found".to_vec())
    );
}

#[test]
fn rm_faster_copies_only_a_module_in_rom() {
    let mut s = session();
    copy_in(&mut s, &Guest::new("Ram")).expect("linked");
    s.write(NAME, b"Ram\r");
    let mut regs = Regs::new([NAME, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let refused = super::commands::rm_faster(&mut s.kernel.os(), &mut regs);
    let Err(crate::os::Fail::Error(block)) = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(s.error_text(block), b"No ROM module has that name");
}

#[test]
fn the_inventory_holds_rom_order_and_the_docs_page_agrees() {
    const PAGE: &str = include_str!("../../../../../../../docs/src/desktop/tairix-bridge.md");
    assert_eq!(INVENTORY.len(), 121);
    assert_eq!(INVENTORY[0].title, "UtilityModule");
    assert_eq!(INVENTORY[0].bcd(), 0x0005_3000);
    let rows: Vec<&str> = PAGE
        .lines()
        .skip_while(|line| !line.starts_with("| Module | Version | Held |"))
        .skip(2)
        .take_while(|line| line.starts_with('|'))
        .collect();
    assert_eq!(rows.len(), INVENTORY.len(), "a row for each module");
    for (row, rom) in rows.iter().zip(&INVENTORY) {
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        let held = match rom.held {
            Held::Provided => "provided",
            Held::Bridged => "bridged",
            Held::Absent => "absent",
        };
        let version = alloc::format!("{}.{:02}", rom.version / 100, rom.version % 100);
        assert_eq!(&cells[1..4], [rom.title, version.as_str(), held], "{row}");
    }
}

#[test]
fn genuine_risc_os_5_lists_the_inventory_s_rom_modules() {
    use crate::corpus::{parse_observations, parse_scenarios, Kind};
    const SCENARIOS: &str = include_str!("../../tests/corpus/modules.scenarios");
    const FIXTURES: &str = include_str!("../../tests/corpus/modules.fixtures");
    let scenarios = parse_scenarios(SCENARIOS).expect("the scenarios");
    let reference = scenarios.iter().find(|s| s.name == "rom-modules");
    assert_eq!(reference.map(|s| s.kind), Some(Kind::Reference));
    let observations = parse_observations(FIXTURES).expect("the fixtures");
    let recorded = observations
        .iter()
        .find(|o| o.name == "rom-modules")
        .expect("recorded");
    // Each module's three shows: OS_Module 20's answer, its title's length as
    // GSTrans copies it, and the CRC of the block it was copied into.
    let rows: Vec<&[alloc::string::String]> = recorded.lines.chunks(3).collect();
    assert!(
        rows.len() > INVENTORY.len(),
        "a row past the main ROM's last"
    );
    for (rom, row) in INVENTORY.iter().zip(&rows) {
        let mut copied = [0xEE; 40];
        copied[..rom.title.len()].copy_from_slice(rom.title.as_bytes());
        copied[rom.title.len()] = 0;
        let listed = alloc::format!("r5=&00000000 r6=&{:08X} v=0", rom.bcd());
        let length = alloc::format!("r2=&{:08X} v=0", rom.title.len());
        let crc = alloc::format!("r0=&{:08X}", crate::text::crc16(0, &copied));
        assert!(row[0].ends_with(&listed), "{}: {}", rom.title, row[0]);
        assert!(row[1].ends_with(&length), "{}: {}", rom.title, row[1]);
        assert!(row[2].ends_with(&crc), "{}: {}", rom.title, row[2]);
    }
    let after = &rows[INVENTORY.len()][0];
    assert!(
        after.contains("v=1") || !after.contains(" r5=&00000000 "),
        "nothing more of the main ROM: {after}"
    );
}

#[test]
fn a_built_in_s_entries_reach_its_rust_from_guest_code() {
    let mut s = session();
    let code = named(&mut s, 18, b"UtilityModule").expect("found").r[3];
    let final_entry = code + (s.peek(code + 8) & 0x7FFF_FFFF);
    s.poke(
        CODE,
        &[
            asm::mov(10, 1),
            asm::add(14, 15, 4),
            crate::host::jump().expect("encoded"),
            final_entry,
            asm::b(CODE + 16, CODE + 16),
        ],
    );
    s.enter(Bits::Bit32, CODE, [0; 13]);
    assert_eq!(s.run(200), Outcome::Budget);
    let state = s.kernel.state();
    assert_eq!(state.pc, CODE + 16, "back to its caller");
    assert_ne!(state.cpsr & psr::V, 0);
    assert_eq!(s.peek(state.user[0]), 0x104);
}

#[test]
fn an_expansion_card_s_module_comes_only_through_a_guest_s_podule_manager() {
    let mut s = session();
    s.write(NAME, b"\r");
    let none = [17, NAME, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        module(&mut s, none),
        Err(0x1E6),
        "no Podule manager, no cards"
    );
    let card = Guest::new("CardMod").with_entries();
    let at = IMAGE + 0x100;
    s.poke(at - 4, &[card.at() + 4]);
    s.write(at, &card.bytes);
    let mut manager = Guest::new("Podule");
    manager.set(Field::SwiChunk, 0x4_0280);
    manager.entry(Field::SwiHandler, |_| {
        alloc::vec![
            asm::cmp_imm(11, 11),
            asm::when(Condition::Eq, asm::mov(0, 0x0300_0000)),
            asm::when(Condition::Eq, asm::mov_pc_lr()),
            asm::cmp_imm(0, 1),
            asm::when(Condition::Eq, asm::mov(2, 0x81)),
            asm::when(Condition::Ne, asm::mov(2, 0x80)),
            asm::ldr(6, 15, 0),
            asm::mov_pc_lr(),
            at,
        ]
    });
    copy_in(&mut s, &manager).expect("linked");
    module(&mut s, none).expect("the card's module linked");
    assert_eq!(s.peek(SEEN + 4), 0x0300_0000, "R11 the card's hardware");
    assert_eq!(
        named(&mut s, 18, b"CardMod").expect("found").r[3],
        at,
        "run in place"
    );
    let other = [17, NAME, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        module(&mut s, other),
        Err(0x107),
        "a chunk that is no module"
    );
}

#[test]
fn a_start_that_is_no_offset_is_the_instruction_at_word_0() {
    use super::reasons::start_entry;
    assert_eq!(start_entry(0x1000, 0x40), 0x1040);
    assert_eq!(
        start_entry(0x1000, 0x0080_0040),
        0x0080_1040,
        "below the top byte"
    );
    assert_eq!(
        start_entry(0x1000, 0x0100_0040),
        0x1000,
        "a bit in the top byte"
    );
    assert_eq!(start_entry(0x1000, 0xEA00_0010), 0x1000, "a branch");
    assert_eq!(start_entry(0x1000, 0x42), 0x1000, "unaligned");
}

#[test]
fn unplugging_a_rom_module_kills_every_copy_of_it() {
    let mut s = session();
    let title = rom_titles(&mut s)
        .into_iter()
        .find(|&title| title != "UtilityModule")
        .expect("a built-in besides the kernel's");
    let line = |s: &mut Session, text: &str| {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(b'\r');
        s.write(NAME, &bytes);
        let (regs, _) = s.user_swi(X | 0x05, [NAME, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(!regs.flag(psr::V), "{text}");
    };
    line(&mut s, &alloc::format!("RMFaster {title}"));
    line(&mut s, &alloc::format!("Unplug {title}"));
    assert_eq!(
        named(&mut s, 18, title.as_bytes()).map(drop),
        Err(0x102),
        "no copy left"
    );
    line(&mut s, &alloc::format!("RMInsert {title}"));
}

#[test]
fn installing_a_key_handler_tells_the_modules() {
    let mut s = session();
    copy_in(&mut s, &listener("KeyEars", 0x48, 0)).expect("linked");
    let handler = IMAGE + 0x800;
    s.poke(handler, &[0, 0, 0, 0, 0, 0, 32, 0, asm::mov_pc_lr()]);
    s.poke(SEEN + 0x4C, &[0]);
    let (old, _) = s.user_swi(X | 0x3E, [handler, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(s.peek(SEEN + 0x4C), 0x44, "Service_KeyHandler");
    s.user_swi(X | 0x3E, [old.r[0], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
}

/// A module titled `title` whose one command, `TairixSame`, writes `mark`
/// at `SEEN + 0x70`.
fn commander(title: &str, mark: u32) -> Guest {
    let mut guest = Guest::new(title);
    let code = guest.words(&[
        asm::mov(1, SEEN),
        asm::mov(0, mark),
        asm::str(0, 1, 0x70),
        asm::mov_pc_lr(),
    ]);
    let table = guest.at();
    guest.bytes.extend_from_slice(b"TairixSame\0");
    guest.bytes.resize(guest.bytes.len().next_multiple_of(4), 0);
    // The code, then up to 255 parameters, no syntax and no help.
    guest.words(&[code, 0x00FF_0000, 0, 0]);
    guest.bytes.extend_from_slice(&[0; 4]);
    guest.set(Field::Commands, table);
    guest
}

#[test]
fn a_command_two_modules_have_is_the_first_s_in_the_module_list() {
    let mut s = session();
    copy_in(&mut s, &commander("Firstly", 1)).expect("linked");
    copy_in(&mut s, &commander("Secondly", 2)).expect("linked");
    named(&mut s, 3, b"Firstly").expect("reinitialised, still first in the list");
    s.write(NAME, b"TairixSame\r");
    let (regs, _) = s.user_swi(X | 0x05, [NAME, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(!regs.flag(psr::V));
    assert_eq!(s.peek(SEEN + 0x70), 1);
}
