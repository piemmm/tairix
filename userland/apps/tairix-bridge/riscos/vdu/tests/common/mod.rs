//! What the VDU drivers' integration tests share: a session with the drivers
//! and their module, the VDU stream written, and whether the drivers still
//! answer.

#![allow(
    dead_code,
    unused_imports,
    reason = "each test crate including this uses the helpers its own tests need"
)]

use std::ops::Range;
use std::sync::LazyLock;

use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{modules_with, session, Session, Setup, DISC, SLOT_BYTES};
use tairix_riscos_kernel::memory::layout::ZERO_PAGE_SIZE;
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::os::{
    screen_mode, CLI, MODULE, READ_DYNAMIC_AREA, READ_MODE_VARIABLE, SCREEN_MODE, SPRITE_OP,
    WRITE_C,
};

/// `OS_Module`'s reasons to claim a block and insert a module there.
const CLAIM: u32 = 6;
const INSERT: u32 = 10;
use tairix_riscos_kernel::swi::X;

/// Where a test writes a mode selector, in the application slot.
pub const SELECTOR_AT: u32 = 0x9000;
/// Where a test writes a command line, in the application slot.
pub const COMMAND_AT: u32 = 0xA000;
/// The values `OS_SpriteOp`'s R0 adds to a reason: a user area's sprite by
/// name, and a sprite by its address.
pub use tairix_riscos_kernel::swi::sprite_op::{BY_NAME as USER, BY_POINTER};

/// The modules a drivers' session has besides the filing system's: theirs,
/// and Draw's, which SpriteExtend's transformed plots call.
static DRIVERS: LazyLock<Vec<&'static Builtin>> = LazyLock::new(|| {
    [
        &tairix_riscos_vdu::MODULES[..],
        &tairix_riscos_draw::MODULES[..],
    ]
    .concat()
});

/// A session with the drivers, their modules and Draw, `keys` to type.
pub fn drivers(keys: &[u8]) -> Session {
    drivers_with(DRIVERS.as_slice(), keys)
}

/// A session with the drivers and `modules`, `keys` to type: for a crate
/// whose modules draw through the drivers.
pub fn drivers_with(modules: &'static [&'static Builtin], keys: &[u8]) -> Session {
    session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules,
        bytes: SLOT_BYTES,
        keys,
        disc: None,
    })
}

/// `bytes` written to the VDU a character at a time.
pub fn vdu(session: &mut Session, bytes: &[u8]) -> Result<(), Fail> {
    for &byte in bytes {
        let mut regs = Regs::new([u32::from(byte), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        session.kernel.os().perform(X | WRITE_C, &mut regs)?;
    }
    Ok(())
}

/// Whether the drivers still read a mode variable.
pub fn answers(session: &mut Session) -> bool {
    let mut regs = Regs::new([u32::MAX, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | READ_MODE_VARIABLE, &mut regs)
        .is_ok()
}

/// SWI `swi`'s X form with R0 onwards `r`, its registers on exit.
pub fn call(session: &mut Session, swi: u32, r: &[u32]) -> Result<Regs, Fail> {
    let mut regs = Regs::default();
    regs.r[..r.len()].copy_from_slice(r);
    session.kernel.os().perform(X | swi, &mut regs)?;
    Ok(regs)
}

/// The number of the error `fail` gives.
pub fn error_number(session: &mut Session, fail: Fail) -> u32 {
    match fail {
        Fail::Error(block) => session.kernel.os().read_word(block).expect("its number"),
        Fail::Unwind(unwind) => panic!("unwound: {unwind:?}"),
    }
}

/// `text` run as a command.
pub fn command(session: &mut Session, text: &str) -> Result<(), Fail> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    session
        .kernel
        .os()
        .write_bytes(COMMAND_AT, &bytes)
        .expect("written");
    call(session, CLI, &[COMMAND_AT]).map(drop)
}

/// A mode of `log2_bpp` bits a pixel at 640 by 480, chosen by selector.
pub fn select_depth(session: &mut Session, log2_bpp: u32) -> Result<(), Fail> {
    select(session, &[1, 640, 480, log2_bpp, u32::MAX, u32::MAX])
}

/// A 640 by 480 mode of 256 colours each its own palette entry.
pub fn select_full_palette(session: &mut Session) -> Result<(), Fail> {
    select(
        session,
        &[1, 640, 480, 3, u32::MAX, 0, 0x80, 3, 255, u32::MAX],
    )
}

/// The mode the selector `words` describe.
pub fn select(session: &mut Session, words: &[u32]) -> Result<(), Fail> {
    session.kernel.os().write_words(SELECTOR_AT, words)?;
    call(session, SCREEN_MODE, &[screen_mode::SELECT, SELECTOR_AT]).map(drop)
}

/// The RMA's free bytes, as `OS_Module` 5 describes them.
pub fn rma_free(session: &mut Session) -> u32 {
    call(session, MODULE, &[5]).expect("described").r[3]
}

/// The host's words a hostile call may run on for: enough for any call that
/// ends soon, while one asked for endless work is stopped.
pub const HOSTILE_CALL_WORDS: u32 = 4;

/// The colour modules, which keep the tables and messages calls leave them
/// until they die: a run that reaches them re-initialises them all before
/// judging what was given back.
pub const COLOUR_MODULES: [&str; 3] = ["ColourTrans", "InverseTable", "BlendTable"];

/// The RMA's bytes in use: grown as claims need, it keeps what it grew by.
pub fn rma_used(session: &mut Session) -> u32 {
    let free = rma_free(session);
    call(session, READ_DYNAMIC_AREA, &[1]).expect("the RMA").r[1] - free
}

/// The kernel's own memory: zero page, the RMA, the system heap, the stacks
/// and the host pages. A call writes where its caller points, there too as
/// on RISC OS, so a harness that judges what the modules give back keeps
/// its hostile pointers out of it.
pub struct KernelMemory(Vec<Range<u32>>);

impl KernelMemory {
    /// `session`'s.
    pub fn of(session: &mut Session) -> Self {
        let mut os = session.kernel.os();
        let zero_page = os.vector_base();
        let layout = os.layout();
        let mut areas = vec![
            zero_page..zero_page + ZERO_PAGE_SIZE,
            layout.rma.clone(),
            layout.system_heap.clone(),
            layout.host.clone(),
        ];
        areas.extend(layout.stacks.all().into_iter().cloned());
        Self(areas)
    }

    /// Whether `address` lies in it.
    pub fn holds(&self, address: u32) -> bool {
        self.0.iter().any(|area| area.contains(&address))
    }
}

/// A sprite area at `area` holding one 8 by 8 sprite of `mode` named as
/// the string at `name`, given its mode's palette where `palette`: the
/// sprite's address.
pub fn lone_sprite(
    session: &mut Session,
    area: u32,
    name: u32,
    mode: u32,
    palette: bool,
) -> Result<u32, Fail> {
    {
        let os = &mut session.kernel.os();
        os.write_word(area, 0x4000)?;
        os.write_word(area + 8, 16)?;
    }
    call(session, SPRITE_OP, &[0x109, area])?;
    call(
        session,
        SPRITE_OP,
        &[0x10F, area, name, u32::from(palette), 8, 8, mode],
    )?;
    Ok(call(session, SPRITE_OP, &[0x118, area, name])?.r[2])
}

/// The palette of `sprite`, the last in `area`, grown to `entries` with its
/// image moved up past them, each new entry steady in colour word `word`.
pub fn grow_palette(
    session: &mut Session,
    area: u32,
    sprite: u32,
    entries: u32,
    word: u32,
) -> Result<(), Fail> {
    const IMAGE: u32 = 32;
    const MASK: u32 = 36;
    let os = &mut session.kernel.os();
    let next = os.read_word(sprite)?;
    let image = os.read_word(sprite + IMAGE)?;
    let held = (image.min(os.read_word(sprite + MASK)?) - 44) / 8;
    let grown = 8 * (entries - held);
    let mut pixels = vec![0; usize::try_from(next - image).expect("a sprite's bytes")];
    os.read_bytes(sprite + image, &mut pixels)?;
    os.write_bytes(sprite + image + grown, &pixels)?;
    for entry in held..entries {
        os.write_word(sprite + 44 + 8 * entry, word)?;
        os.write_word(sprite + 48 + 8 * entry, word)?;
    }
    for offset in [0, IMAGE, MASK] {
        let held = os.read_word(sprite + offset)?;
        os.write_word(sprite + offset, held + grown)?;
    }
    let free = os.read_word(area + 12)?;
    os.write_word(area + 12, free + grown)
}

/// Inserts the guest module `image` from a block claimed for it: its base,
/// where it runs.
pub fn insert_module(session: &mut Session, image: &[u8]) -> Result<u32, Fail> {
    let os = &mut session.kernel.os();
    let length = u32::try_from(image.len()).unwrap_or(u32::MAX);
    let mut claim = Regs::new([CLAIM, 0, 0, length, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | MODULE, &mut claim)?;
    let base = claim.r[2];
    os.write_bytes(base, image)?;
    let mut insert = Regs::new([INSERT, base, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | MODULE, &mut insert).map(|()| base)
}

/// UKSWIV claimed by guest code that counts the calls it is given and
/// returns from none, so a SWI no module provides runs until the host's
/// word: the address of the count.
pub fn unknown_swis_held(session: &mut Session) -> u32 {
    const HELD: [u32; 5] = [
        0,           // the count
        0xE51F_C00C, // LDR R12, [PC, #-12]
        0xE28C_C001, // ADD R12, R12, #1
        0xE50F_C014, // STR R12, [PC, #-20]
        0xEAFF_FFFE, // B .
    ];
    let os = &mut session.kernel.os();
    let bytes = u32::try_from(size_of_val(&HELD)).expect("a few words");
    let count = os.rma_claim(bytes).expect("room");
    os.write_words(count, &HELD).expect("written");
    let mut claim = Regs::default();
    claim.r[..3].copy_from_slice(&[tairix_riscos_kernel::vectors::UKSWI.0, count + 4, 0]);
    os.perform(X | tairix_riscos_kernel::swi::os::CLAIM, &mut claim)
        .expect("claimed");
    count
}

/// The drivers' modules with the filing system's.
pub static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[DRIVERS.as_slice()]));

/// A session with [`MODULES`] and a disc holding `files`, each a leaf name
/// and its bytes.
pub fn with_files(files: &[(&str, &[u8])]) -> Session {
    let mut tree = Tree::new().dir(DISC);
    for (name, bytes) in files {
        tree = tree.file(&format!("{DISC}/{name},fff"), bytes);
    }
    session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: b"",
        disc: Some(&tree),
    })
}

/// The code of a routine, entered with R14 its return and a stack in R13,
/// that disarms the hook calling it by writing nought to `slot`, then issues
/// SWI `swi`'s X form with R0 to R7 `registers`: every register kept, V
/// clear on return, so whatever called the hook carries on as before.
pub fn once_routine(slot: u32, swi: u32, registers: [u32; 8]) -> Vec<u32> {
    let mut code = vec![
        0xE92D_5FFF, // STMFD R13!, {R0-R12, R14}
        0xE28F_C014, // ADD R12, PC, #20: the words below
        0xE89C_01FF, // LDMIA R12, {R0-R8}
        0xE3A0_9000, // MOV R9, #0
        0xE588_9000, // STR R9, [R8]: the hook disarmed
        0xEF00_0000 | ((X | swi) & 0x00FF_FFFF),
        0xE150_0000, // CMP R0, R0: V clear
        0xE8BD_9FFF, // LDMFD R13!, {R0-R12, PC}
    ];
    code.extend(registers);
    code.push(slot);
    code
}

/// `words` written from `at`.
pub fn write_words(session: &mut Session, at: u32, words: &[u32]) {
    session.kernel.os().write_words(at, words).expect("written");
}

/// The `count` words from `at`.
pub fn read_words(session: &mut Session, at: u32, count: u32) -> Vec<u32> {
    let mut words = vec![0; count as usize];
    session
        .kernel
        .os()
        .read_words(at, &mut words)
        .expect("read");
    words
}

/// A hook's handler: the routine whose address its slot holds called with
/// R14 its return, unless the slot holds nought, and every register kept.
const HOOK: [u32; 7] = [
    0xE92D_4001, // STMFD R13!, {R0, R14}
    0xE59F_000C, // LDR R0, slot
    0xE330_0000, // TEQ R0, #0
    0x11A0_E00F, // MOVNE R14, PC
    0x11A0_F000, // MOVNE PC, R0
    0xE8BD_8001, // LDMFD R13!, {R0, PC}
    0,           // slot
];

/// A 32-bit guest module image of a 13-word header, its `code` from &34,
/// then its flags word, then its title.
fn module_image(entries: [u32; 4], code: &[u32], title: &[u8]) -> (Vec<u8>, u32) {
    let after_code = 0x34 + 4 * u32::try_from(code.len()).expect("short");
    let title_at = after_code + 4;
    let [start, init, final_, service] = entries;
    let header = [
        start, init, final_, service, title_at, title_at, 0, 0, 0, 0, 0, 0, after_code,
    ];
    let mut image: Vec<u8> = header
        .iter()
        .chain(code)
        .chain(&[1])
        .flat_map(|w| w.to_le_bytes())
        .collect();
    image.extend_from_slice(title);
    image.push(0);
    image.resize(image.len().next_multiple_of(4), 0);
    (image, after_code - 4)
}

/// A module claiming `PaletteV` with a [`HOOK`], and the offset of the
/// hook's slot from the module's base.
pub fn palette_v_hook() -> (Vec<u8>, u32) {
    let init = [
        0xE92D_4000, // STMFD R13!, {R14}
        0xE3A0_0000 | tairix_riscos_kernel::vectors::PALETTE.0,
        0xE28F_1008, // ADR R1, the hook below
        0xE3A0_2000, // MOV R2, #0
        0xEF00_0000 | X | tairix_riscos_kernel::swi::os::CLAIM,
        0xE8BD_8000, // LDMFD R13!, {PC}
    ];
    let code: Vec<u32> = init.iter().chain(&HOOK).copied().collect();
    module_image([0, 0x34, 0, 0], &code, b"PalHook")
}

/// A module whose service handler runs a [`HOOK`] on service `number`,
/// claiming nothing, and the offset of the hook's slot from its base.
pub fn service_hook(number: u32) -> (Vec<u8>, u32) {
    let number = u8::try_from(number).expect("a service TEQ's immediate names");
    let code: Vec<u32> = [
        0xE331_0000 | u32::from(number), // TEQ R1, #number
        0x11A0_F00E,                     // MOVNE PC, R14
    ]
    .iter()
    .chain(&HOOK)
    .copied()
    .collect();
    module_image([0, 0, 0, 0x34], &code, b"SvcHook")
}
