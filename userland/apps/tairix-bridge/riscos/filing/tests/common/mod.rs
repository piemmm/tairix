//! What the filing crate's integration tests share.

#![allow(
    dead_code,
    reason = "each test crate including this uses the helpers its own tests need"
)]

pub mod programs;
pub mod served;

use tairix_riscos_filing::filecore::{code, numbered};
use tairix_riscos_filing::ramfs;
use tairix_riscos_kernel::corpus::StillHost;
use tairix_riscos_kernel::memory::layout::RISC_OS_5;
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::os::{Config, Fail, Host, Kernel, Os, Outcome, Regs};
use tairix_riscos_kernel::ramdisc::AREA;
use tairix_riscos_kernel::swi::os::{CHANGE_DYNAMIC_AREA, CLI, FILE, GET_ENV};
use tairix_riscos_kernel::swi::X;

/// FileCore's "not found" as RamFS reports it: FileCore's errors beside
/// RamFS's number.
pub const FILECORE_NOT_FOUND: u32 = numbered(ramfs::NUMBER as u32, code::NOT_FOUND);

/// A session booted afresh with this crate's modules.
pub fn kernel() -> Kernel {
    kernel_on(Box::new(StillHost))
}

/// As [`kernel`], on `host`.
pub fn kernel_on(host: Box<dyn Host>) -> Kernel {
    configured(host, 256 * 1024 * 1024)
}

/// As [`kernel`], in a machine of `bytes`.
pub fn kernel_in(bytes: u64) -> Kernel {
    configured(Box::new(StillHost), bytes)
}

fn configured(host: Box<dyn Host>, bytes: u64) -> Kernel {
    let config = Config {
        layout: &RISC_OS_5,
        ceiling: Ceiling::derive(bytes),
        memo_pages: 16,
        host,
        vdu: None,
        modules: &tairix_riscos_filing::MODULES,
        zone: tairix_tz::Zone::UTC,
        desktop_font: tairix_riscos_kernel::desktop::DesktopFont::SHIPPED,
        pressure: &tairix_reclaim::pressure::Unpressured,
        log: &tairix_log::DiscardSink,
        cache_key: tairix_hash::HashSeed::UNKEYED,
    };
    Kernel::new(config).expect("a kernel")
}

/// `swi` with `args` from R0: its registers, or its error's number and text.
pub fn call(os: &mut Os<'_>, swi: u32, args: &[u32]) -> Result<Regs, (u32, String)> {
    let mut regs = Regs::default();
    regs.r[..args.len()].copy_from_slice(args);
    match os.perform(X | swi, &mut regs) {
        Ok(()) => Ok(regs),
        Err(Fail::Error(block)) => {
            let number = os.read_word(block).expect("its number");
            let text = os.read_zstring(block + 4, 252).expect("its text");
            Err((number, String::from_utf8_lossy(&text).into_owned()))
        }
        Err(Fail::Unwind(unwind)) => panic!("unwound: {unwind:?}"),
    }
}

/// `text` in the RMA, terminated.
pub fn text(os: &mut Os<'_>, text: &[u8]) -> u32 {
    let size = u32::try_from(text.len() + 1).expect("short");
    let at = os.rma_claim(size).expect("room");
    os.write_terminated(at, text, 0, text.len() + 1)
        .expect("written");
    at
}

/// `bytes` in the RMA.
pub fn block(os: &mut Os<'_>, bytes: &[u8]) -> u32 {
    let at = os
        .rma_claim(u32::try_from(bytes.len().max(4)).expect("short"))
        .expect("room");
    os.write_bytes(at, bytes).expect("written");
    at
}

/// `count` bytes from `at`.
pub fn read(os: &mut Os<'_>, at: u32, count: usize) -> Vec<u8> {
    let mut out = vec![0; count];
    os.read_bytes(at, &mut out).expect("readable");
    out
}

/// What `OS_File` 5 reads of `name`: its type, load and execution words and
/// length.
pub fn info(os: &mut Os<'_>, name: &[u8]) -> Result<(u32, u32, u32, u32), (u32, String)> {
    let name = text(os, name);
    call(os, FILE, &[5, name]).map(|regs| (regs.r[0], regs.r[2], regs.r[3], regs.r[4]))
}

/// `bytes` saved as `name`, of type `filetype`.
pub fn save(
    os: &mut Os<'_>,
    name: &[u8],
    filetype: u32,
    bytes: &[u8],
) -> Result<(), (u32, String)> {
    let name = text(os, name);
    let data = block(os, bytes);
    let end = data + u32::try_from(bytes.len()).expect("short");
    call(os, FILE, &[10, name, filetype, 0, data, end]).map(drop)
}

/// The file `name`, loaded.
pub fn load(os: &mut Os<'_>, name: &[u8], length: usize) -> Result<Vec<u8>, (u32, String)> {
    let name = text(os, name);
    let at = os
        .rma_claim(u32::try_from(length.max(4)).expect("short"))
        .expect("room");
    call(os, FILE, &[255, name, at, 0])?;
    Ok(read(os, at, length))
}

/// The session given a RAM disc of 256K.
pub fn ram_disc(os: &mut Os<'_>) {
    call(os, CHANGE_DYNAMIC_AREA, &[AREA, 0x4_0000]).expect("a RAM disc");
}

/// A session with a RAM disc and an application slot of `bytes`, as the
/// front end gives a program one: its memory limit.
pub fn session(kernel: &mut Kernel, bytes: u32) -> u32 {
    tairix_riscos_filing::session::application_slot(kernel, bytes);
    let mut os = kernel.os();
    ram_disc(&mut os);
    call(&mut os, GET_ENV, &[]).expect("the environment").r[1]
}

/// The ARM words of a program that exits at once: `OS_Exit` with no code.
pub const EXITS: [u8; 16] = [
    0x00, 0x00, 0xA0, 0xE3, 0x00, 0x10, 0xA0, 0xE3, 0x00, 0x20, 0xA0, 0xE3, 0x11, 0x00, 0x00, 0xEF,
];
/// One that raises an error: `ADR R0, block`, `SWI OS_GenerateError`, then
/// the block.
pub const FAILS: [u8; 20] = [
    0x00, 0x00, 0x8F, 0xE2, 0x2B, 0x00, 0x00, 0xEF, 0x45, 0x23, 0x01, 0x00, 0x50, 0x72, 0x6F, 0x62,
    0x65, 0x00, 0x00, 0x00,
];

/// The variable `name`'s value, none where it is unset.
pub fn variable(os: &mut Os<'_>, name: &[u8]) -> Option<Vec<u8>> {
    os.variable_text(name).expect("readable")
}

/// `OS_CLI` on `line`, as it comes back.
pub fn command(os: &mut Os<'_>, line: &[u8]) -> Result<(), Fail> {
    let line = text(os, line);
    let mut regs = Regs::default();
    regs.r[0] = line;
    os.perform(X | CLI, &mut regs)
}

/// The session run on until its program ends.
pub fn run_out(kernel: &mut Kernel) -> Outcome {
    kernel.run(1 << 24)
}
