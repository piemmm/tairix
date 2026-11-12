//! What the TAIRiX DOS tests share: a FAT12 system diskette made through
//! `lib/fat`, programs assembled by the crate's encoder, and a booted system
//! whose idle shell a test makes calls from.

use super::formats::Diskette;
use super::*;
use crate::asm::{Asm, Reg16, Reg8};
use crate::bios::run_until;
use crate::chips::keyboard::Scan;
use crate::cpu::{flag, gpr, Bus, Regs, Seg};
use crate::machine::{Machine, Template};
use crate::storage::floppy::Floppy;
use crate::storage::hdc::{DiskType, LowLevel};
use crate::storage::{BlockDevice, MediaError, MemoryDisk};
use crate::time::{Nanos, NANOS_PER_MILLISECOND, NANOS_PER_SECOND};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use tairix_fat::boot::{Bpb, Kind};
use tairix_fat::format::{self, Medium, Shape};
use tairix_partition::mbr::Geometry;

pub(crate) const SECOND: Nanos = NANOS_PER_SECOND;

/// A 360 KiB diskette's volume.
pub(crate) fn diskette() -> Bpb {
    Diskette::K360.volume(0x1986_0310)
}

/// A file on a test volume: its name as an entry holds it, and its bytes.
pub(crate) struct File<'a> {
    pub(crate) name: &'a [u8; 11],
    pub(crate) bytes: &'a [u8],
}

/// A 360 KiB TAIRiX DOS system diskette holding `files` in its root.
pub(crate) fn image(files: &[File<'_>]) -> Vec<u8> {
    image_of(&diskette(), files)
}

/// Volume `volume` holding `files` in its root.
pub(crate) fn image_of(volume: &Bpb, files: &[File<'_>]) -> Vec<u8> {
    let names: Vec<String> = files
        .iter()
        .map(|file| {
            let mut spelt = [0u8; 13];
            let len = name::spell(file.name, &mut spelt);
            String::from_utf8(spelt[..len].to_vec()).unwrap()
        })
        .collect();
    let entries: Vec<volume::File<'_>> = files
        .iter()
        .zip(&names)
        .map(|(file, path)| volume::File {
            path,
            bytes: file.bytes,
            attributes: 0,
        })
        .collect();
    volume::volume(volume, None, &entries, tairix_fat::stamp::Stamp::NONE).unwrap()
}

/// The bytes of 360 KiB diskette `floppy`, as its drive reads them.
pub(crate) fn diskette_bytes(floppy: &mut Floppy) -> Vec<u8> {
    let mut image = vec![0u8; 720 * 512];
    for (lba, sector) in image.as_chunks_mut::<512>().0.iter_mut().enumerate() {
        let (cylinder, in_cylinder) = (lba / 18, lba % 18);
        let (cylinder, head) = (u8::try_from(cylinder).unwrap(), u8::from(in_cylinder >= 9));
        floppy
            .read(cylinder, head, in_cylinder % 9, sector)
            .unwrap();
    }
    image
}

/// Shrinks the shell to 64 KiB, so a child has room.
pub(crate) fn make_room(dos: &mut Dos) {
    let shell = dos.psp;
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x4A00;
        r.gpr[gpr::BX] = 0x1000;
        r.set_sreg(Seg::Es, shell);
    });
}

/// An EXEC parameter block at `block` for a program given no tail and no
/// FCBs; ES:BX for it.
pub(crate) fn exec_block(dos: &mut Dos, block: u16) {
    let shell = dos.psp;
    let tail = block + 0x20;
    let mut parameters = [0u8; 14];
    for (at, offset) in [(2, tail), (6, tail + 0x10), (10, tail + 0x10)] {
        parameters[at..at + 2].copy_from_slice(&offset.to_le_bytes());
        parameters[at + 2..at + 4].copy_from_slice(&shell.to_le_bytes());
    }
    dos.put(block, &parameters);
    dos.put(tail, &[0, 0x0D]);
    dos.put(tail + 0x10, &[0; 16]);
}

/// Volume image `image`'s parameters, and the entry of file `name` in its
/// root.
pub(crate) fn root_entry(
    image: &[u8],
    name: &[u8; 11],
) -> Option<(Bpb, tairix_fat::dir::ShortEntry)> {
    let bpb = Bpb::parse(image.get(..512)?.try_into().ok()?)?;
    let root = usize::try_from(bpb.root_start()).ok()?;
    let entries = image.get(root..root + usize::from(bpb.root_entries) * 32)?;
    let entry = entries
        .as_chunks::<32>()
        .0
        .iter()
        .map(tairix_fat::dir::ShortEntry::decode)
        .find(|entry| &entry.name == name)?;
    Some((bpb, entry))
}

/// The sector of volume image `image` holding byte `offset` of root file
/// `name`, whose clusters run on from its first, as a test volume's do.
pub(crate) fn sector_of(image: &[u8], name: &[u8; 11], offset: u32) -> Option<u64> {
    let (bpb, entry) = root_entry(image, name)?;
    let cluster = u64::from(entry.cluster.checked_sub(2)?);
    let sectors = bpb.first_data_sector() + cluster * u64::from(bpb.sectors_per_cluster);
    Some(sectors + u64::from(offset / u32::from(bpb.bytes_per_sector)))
}

/// The file `name` in volume image `image`'s root, read as one cluster.
pub(crate) fn root_file(image: &[u8], name: &[u8; 11]) -> Option<Vec<u8>> {
    let (bpb, entry) = root_entry(image, name)?;
    let sector = usize::from(bpb.bytes_per_sector);
    let cluster = usize::try_from(entry.cluster).ok()?.checked_sub(2)?;
    let at = usize::try_from(bpb.first_data_sector()).ok()? * sector
        + cluster * sector * usize::from(bpb.sectors_per_cluster);
    image
        .get(at..at + usize::try_from(entry.size).ok()?)
        .map(<[u8]>::to_vec)
}

/// A medium whose reads fail where `fails` says, given the sector and how
/// often it has been read before.
struct Unreliable {
    disk: MemoryDisk,
    reads: Vec<u8>,
    fails: Box<dyn Fn(u64, u8) -> bool>,
}

impl BlockDevice for Unreliable {
    fn sectors(&self) -> u64 {
        self.disk.sectors()
    }

    fn read_only(&self) -> bool {
        self.disk.read_only()
    }

    fn read(&mut self, sector: u64, out: &mut [u8; 512]) -> Result<(), MediaError> {
        if let Some(reads) = usize::try_from(sector)
            .ok()
            .and_then(|n| self.reads.get_mut(n))
        {
            let before = *reads;
            *reads = reads.saturating_add(1);
            if (self.fails)(sector, before) {
                return Err(MediaError::Failed);
            }
        }
        self.disk.read(sector, out)
    }

    fn write(&mut self, sector: u64, data: &[u8; 512]) -> Result<(), MediaError> {
        self.disk.write(sector, data)
    }
}

/// A diskette of `image` whose reads fail where `fails` says.
pub(crate) fn unreliable_floppy(
    image: Vec<u8>,
    fails: impl Fn(u64, u8) -> bool + 'static,
) -> Floppy {
    let reads = vec![0; image.len() / 512];
    let disk = MemoryDisk::new(image, false).unwrap();
    Floppy::raw(Box::new(Unreliable {
        disk,
        reads,
        fails: Box::new(fails),
    }))
    .unwrap()
}

/// The blocks of memory `owner` holds, by their arena headers' segments,
/// as the kernel's chain links them.
pub(crate) fn blocks_of(m: &Machine, owner: u16) -> Vec<u16> {
    let peek16 = |segment: u16, offset: u16| {
        let byte = |at: u16| u16::from(m.board().peek(crate::cpu::linear(segment, at)));
        byte(offset) | byte(offset + 1) << 8
    };
    let first =
        u16::try_from(i32::from(data::SYSVARS) + i32::from(data::sysvars::FIRST_MCB)).unwrap();
    let mut at = kernel16(m, first);
    let mut held = Vec::new();
    for _ in 0..0x1000 {
        if peek16(at, memory::mcb::OWNER) == owner {
            held.push(at);
        }
        if m.board().peek(crate::cpu::linear(at, memory::mcb::KIND)) == memory::mcb::LAST {
            break;
        }
        at = at
            .wrapping_add(peek16(at, memory::mcb::SIZE))
            .wrapping_add(1);
    }
    held
}

pub(crate) fn floppy(image: Vec<u8>) -> Floppy {
    Floppy::raw(Box::new(MemoryDisk::new(image, false).unwrap())).unwrap()
}

/// The same, write-protected.
pub(crate) fn protected_floppy(image: Vec<u8>) -> Floppy {
    Floppy::raw(Box::new(MemoryDisk::new(image, true).unwrap())).unwrap()
}

/// A COM program: `program` at offset `100h`.
pub(crate) fn com(program: impl FnOnce(&mut Asm)) -> Vec<u8> {
    let mut asm = Asm::new(0x100);
    program(&mut asm);
    asm.finish().unwrap()
}

/// Stops with every interrupt masked; the V20 leaves HLT on a pending
/// interrupt even with IF clear, so it halts in a loop.
pub(crate) fn stop(asm: &mut Asm) {
    let here = asm.label();
    asm.mov_imm8(Reg8::Al, 0xFF);
    asm.out_al(0x21);
    asm.cli();
    asm.bind(&here);
    asm.hlt();
    asm.jmp_short(&here);
}

/// Prints `$`-ended text through function `09h`.
pub(crate) fn print(asm: &mut Asm, text: &[u8]) {
    let after = asm.label();
    let string = asm.label();
    asm.mov_label(Reg16::Dx, &string);
    asm.mov_imm8(Reg8::Ah, 0x09);
    asm.int(0x21);
    asm.jmp(&after);
    asm.bind(&string);
    asm.bytes(text);
    asm.bind(&after);
}

/// A machine with TAIRiX DOS's ROM, booted from `files`' diskette and run
/// until the shell has stopped, or `limit` has passed.
pub(crate) fn booted(files: &[File<'_>], limit: Nanos) -> Machine {
    let config = Template::IbmPc.config();
    let mut m = crate::bios::machine(&config, [None, None]).unwrap();
    m.board_mut()
        .insert_floppy(0, floppy(image(files)))
        .unwrap();
    let mut deadline = SECOND;
    while deadline <= limit {
        run_until(&mut m, deadline);
        if halted(&m) {
            break;
        }
        deadline += SECOND / 2;
    }
    m
}

/// Whether the CPU sits at a HLT with interrupts masked.
pub(crate) fn halted(m: &Machine) -> bool {
    let regs = m.cpu().regs();
    let at = crate::cpu::linear(regs.sreg(crate::cpu::Seg::Cs), regs.ip.wrapping_sub(1));
    !regs.flag(crate::cpu::flag::IF) && m.board().peek(at) == 0xF4
}

/// Where the CPU and the boot stand, for a failure's message.
pub(crate) fn state(m: &Machine) -> String {
    let regs = m.cpu().regs();
    let kernel = |offset: u16| {
        m.board()
            .peek(u32::from(data::SEGMENT) * 16 + u32::from(offset))
    };
    let depth = kernel(data::SDA + data::sda::DEPTH);
    let frames: Vec<(Option<engine::Id>, u8)> = (0..u16::from(depth.min(data::DEPTH)))
        .map(|n| {
            let at = data::SDA + data::sda::FRAMES + n * data::FRAME;
            (engine::Id::of(kernel(at)), kernel(at + 1))
        })
        .collect();
    alloc::format!(
        "{regs:X?}\nstage {} error {:02X}{:02X} roots {} depth {depth} frames {frames:?}\n",
        kernel(data::BOOT + data::boot::STAGE),
        kernel(data::SDA + data::sda::ERROR + 1),
        kernel(data::SDA + data::sda::ERROR),
        kernel(data::SDA + data::sda::ROOTS),
    )
}

/// The colour text screen, a line a row.
pub(crate) fn screen(m: &Machine) -> String {
    let mut out = String::new();
    for row in 0..25u32 {
        let line: String = (0..80u32)
            .map(|column| char::from(m.board().peek(0xB_8000 + (row * 80 + column) * 2)))
            .collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// A booted TAIRiX DOS whose shell sits idle with interrupts on, which a
/// test makes calls into from a stub in the shell's own segment.
pub(crate) struct Dos {
    pub(crate) m: Machine,
    pub(crate) psp: u16,
}

/// Where the call stub lies in the shell's segment: INT 21h, then a jump to
/// itself the test waits for.
pub(crate) const STUB: u16 = 0x0200;
/// Where a test puts its strings and buffers there.
pub(crate) const DATA: u16 = 0x0400;

/// A 360 KiB raw diskette image whose boot sector is `program`.
pub(crate) fn boot_image(program: impl FnOnce(&mut Asm)) -> Vec<u8> {
    let mut asm = Asm::new(0x7C00);
    program(&mut asm);
    let code = asm.finish().unwrap();
    let mut image = vec![0u8; 360 * 1024];
    image[..code.len()].copy_from_slice(&code);
    image[510] = 0x55;
    image[511] = 0xAA;
    image
}

/// The idle shell: it waits with interrupts on, for ever.
pub(crate) fn idle(asm: &mut Asm) {
    let here = asm.label();
    asm.sti();
    asm.bind(&here);
    asm.hlt();
    asm.jmp_short(&here);
}

/// Runs `m` for each key event in turn, a tenth of a second apart.
pub(crate) fn press(m: &mut Machine, keys: &[(Scan, bool)]) {
    for &(key, down) in keys {
        let now = m.now();
        run_until(m, now + SECOND / 10);
        m.board_mut().key(key, down);
    }
}

/// Runs `m` until `seen` holds of it, for at most `within` of emulated time.
pub(crate) fn run_while_not(m: &mut Machine, within: Nanos, seen: impl Fn(&Machine) -> bool) {
    let deadline = m.now() + within;
    while !seen(m) {
        assert!(
            m.now() < deadline,
            "the machine never got there:\n{}{}",
            screen(m),
            state(m)
        );
        let now = m.now();
        run_until(m, now + SECOND / 20);
    }
}

/// A machine `config` describes about to boot from a diskette holding just
/// `files`.
pub(crate) fn bare(config: &crate::board::BoardConfig, files: &[File<'_>]) -> Machine {
    bare_on(config, image(files))
}

/// The same, booting from the diskette `image`.
pub(crate) fn bare_on(config: &crate::board::BoardConfig, image: Vec<u8>) -> Machine {
    let mut m = machine_of(config, [None, None]);
    m.board_mut().insert_floppy(0, floppy(image)).unwrap();
    m
}

/// The machine `config` describes, its firmware for fixed disks of `kinds`,
/// its drives empty.
pub(crate) fn machine_of(
    config: &crate::board::BoardConfig,
    kinds: [Option<DiskType>; 2],
) -> Machine {
    crate::bios::machine(config, kinds).unwrap()
}

/// The fixed disk the tests attach: a megabyte on an MFM drive of 64
/// cylinders, two heads and 17 sectors a track.
pub(crate) const FIXED_DISK: DiskType = DiskType {
    cylinders: 64,
    heads: 2,
    track_to_track: 3 * NANOS_PER_MILLISECOND,
    average_seek: 85 * NANOS_PER_MILLISECOND,
};
/// Its sectors, and where its one partition starts: the second track.
const FIXED_SECTORS: u32 = 64 * 2 * 17;
pub(crate) const PARTITION_START: u32 = 17;

/// The volume the fixed disk's partition holds when formatted: FAT12, as a
/// partition this small is, with a hard disk's root.
pub(crate) fn fixed_volume(serial: u32) -> Bpb {
    let medium = Medium {
        sectors: FIXED_SECTORS - PARTITION_START,
        bytes_per_sector: 512,
        sectors_per_track: 17,
        heads: 2,
        hidden_sectors: PARTITION_START,
        media: format::FIXED_MEDIA,
    };
    let shape = Shape {
        kind: None,
        sectors_per_cluster: None,
        root_entries: 0,
        serial,
        label: None,
    };
    format::plan(&medium, &shape).unwrap()
}

/// The fixed disk's image: TAIRiX DOS's master boot record with one active
/// DOS partition from its second track to its end, holding `volume` — or
/// nothing, as `FDISK` leaves one.
pub(crate) fn fixed_image(volume: Option<&[u8]>) -> Vec<u8> {
    let sectors = FIXED_SECTORS - PARTITION_START;
    let record =
        super::setup::partitioned(Kind::Fat12, PARTITION_START, sectors, Geometry::new(2, 17))
            .unwrap();
    let mut image = vec![0u8; usize::try_from(FIXED_SECTORS).unwrap() * 512];
    image[..512].copy_from_slice(&record);
    if let Some(volume) = volume {
        let at = usize::try_from(PARTITION_START).unwrap() * 512;
        image[at..at + volume.len()].copy_from_slice(volume);
    }
    image
}

/// A fixed disk a test attaches: its drive, what it holds, and the tracks
/// marked bad on it.
pub(crate) struct Fixed {
    pub(crate) kind: DiskType,
    pub(crate) device: Box<dyn BlockDevice>,
    pub(crate) bad: Vec<u32>,
}

/// A PC/XT about to boot from its fixed disks, the first two of `disks`,
/// its diskette drives empty.
pub(crate) fn fixed_disks(disks: Vec<Fixed>) -> Machine {
    let kinds = [
        disks.first().map(|disk| disk.kind),
        disks.get(1).map(|disk| disk.kind),
    ];
    let mut m = machine_of(&Template::IbmXt.config(), kinds);
    for (n, disk) in disks.into_iter().enumerate() {
        let low_level = LowLevel {
            interleave: 3,
            bad_tracks: disk.bad,
        };
        m.board_mut()
            .attach_fixed(n, disk.kind, disk.device, low_level)
            .unwrap();
    }
    m
}

/// A PC/XT about to boot from its fixed disk, which holds `image` with the
/// tracks `bad` marked bad, its diskette drives empty.
pub(crate) fn fixed_boot(image: Vec<u8>, bad: &[u32]) -> Machine {
    let device = Box::new(MemoryDisk::new(image, false).unwrap());
    fixed_disks(vec![Fixed {
        kind: FIXED_DISK,
        device,
        bad: bad.to_vec(),
    }])
}

/// The same about to boot from the diskette `diskette` instead.
pub(crate) fn fixed_machine(image: Vec<u8>, bad: &[u32], diskette: Vec<u8>) -> Machine {
    let mut m = fixed_boot(image, bad);
    m.board_mut().insert_floppy(0, floppy(diskette)).unwrap();
    m
}

/// `m` booted from a diskette of `volume`'s shape holding `COMMAND.COM` and
/// `files`, its prompt showing.
pub(crate) fn shell_in(mut m: Machine, volume: &Bpb, files: &[File<'_>]) -> Machine {
    let shell = super::shell::image().unwrap();
    let mut all = alloc::vec![File {
        name: b"COMMAND COM",
        bytes: &shell
    }];
    all.extend(files.iter().map(|f| File {
        name: f.name,
        bytes: f.bytes,
    }));
    m.board_mut()
        .insert_floppy(0, floppy(image_of(volume, &all)))
        .unwrap();
    settle(&mut m);
    m
}

impl Dos {
    /// TAIRiX DOS booted from a diskette holding `files` and an idle shell.
    pub(crate) fn boot(files: &[File<'_>]) -> Self {
        Self::shell_of(Self::machine(files))
    }

    /// A machine about to boot from a diskette holding `files` and an idle
    /// `COMMAND.COM`.
    pub(crate) fn machine(files: &[File<'_>]) -> Machine {
        Self::machine_with(&Template::IbmPc.config(), files)
    }

    /// The same, of the machine `config` describes.
    pub(crate) fn machine_with(config: &crate::board::BoardConfig, files: &[File<'_>]) -> Machine {
        let shell = com(idle);
        let mut all = vec![File {
            name: b"COMMAND COM",
            bytes: &shell,
        }];
        all.extend(files.iter().map(|f| File {
            name: f.name,
            bytes: f.bytes,
        }));
        bare(config, &all)
    }

    /// TAIRiX DOS on a PC/XT whose fixed disk holds `image`, booted from a
    /// diskette holding an idle shell.
    pub(crate) fn fixed(image: Vec<u8>) -> Self {
        let shell = com(idle);
        Self::shell_of(fixed_machine(
            image,
            &[],
            image_of(
                &diskette(),
                &[File {
                    name: b"COMMAND COM",
                    bytes: &shell,
                }],
            ),
        ))
    }

    /// `m` run until its shell idles.
    pub(crate) fn shell_of(mut m: Machine) -> Self {
        let idling = |m: &Machine| {
            let psp = kernel16(m, data::SDA + data::sda::PSP);
            let regs = m.cpu().regs();
            regs.sreg(Seg::Cs) == psp && (0x101..0x104).contains(&regs.ip)
        };
        run_while_not(&mut m, 30 * SECOND, idling);
        let psp = kernel16(&m, data::SDA + data::sda::PSP);
        Self { m, psp }
    }

    /// Makes INT 21h with the registers `set` gives, from the shell's stub;
    /// the registers it returned with.
    pub(crate) fn call(&mut self, set: impl FnOnce(&mut Regs)) -> Regs {
        self.int(0x21, set)
    }

    /// Makes INT `vector` the same way.
    pub(crate) fn int(&mut self, vector: u8, set: impl FnOnce(&mut Regs)) -> Regs {
        self.run(&[0xCD, vector], set, &[])
    }

    /// Runs `code` from the stub with the registers `set` gives, the key
    /// events `keys` — each key, and whether it goes down — coming a tenth of
    /// a second apart, until it reaches the jump to itself after it.
    pub(crate) fn run(
        &mut self,
        code: &[u8],
        set: impl FnOnce(&mut Regs),
        keys: &[(Scan, bool)],
    ) -> Regs {
        let end = self.begin(code, set);
        press(&mut self.m, keys);
        self.finish(end)
    }

    /// Starts `code` from the stub with the registers `set` gives, leaving
    /// the machine to be run; where the stub ends.
    pub(crate) fn begin(&mut self, code: &[u8], set: impl FnOnce(&mut Regs)) -> u16 {
        let mut stub = Vec::from(code);
        stub.extend_from_slice(&[0xEB, 0xFE]);
        let end = STUB + u16::try_from(code.len()).unwrap();
        self.put(STUB, &stub);
        self.m.cpu_mut().reset();
        let psp = self.psp;
        let regs = self.m.cpu_mut().regs_mut();
        for seg in [Seg::Cs, Seg::Ss, Seg::Ds, Seg::Es] {
            regs.set_sreg(seg, psp);
        }
        regs.ip = STUB;
        regs.gpr[gpr::SP] = 0xFFF0;
        regs.set_flag(flag::IF, true);
        set(regs);
        end
    }

    /// Runs the code [`Self::begin`] started until it reaches `end`; the
    /// registers it got there with.
    pub(crate) fn finish(&mut self, end: u16) -> Regs {
        let begun = self.m.now();
        loop {
            let now = self.m.now();
            run_until(&mut self.m, now + SECOND / 20);
            let regs = self.m.cpu().regs();
            if regs.sreg(Seg::Cs) == self.psp && regs.ip == end {
                return *regs;
            }
            assert!(
                self.m.now() - begun < 30 * SECOND,
                "the call never returned:\n{}{}",
                screen(&self.m),
                state(&self.m)
            );
        }
    }

    /// Writes `bytes` at `offset` of the shell's segment.
    pub(crate) fn put(&mut self, offset: u16, bytes: &[u8]) {
        for (at, &byte) in (offset..).zip(bytes) {
            let linear = crate::cpu::linear(self.psp, at);
            self.m.board_mut().write(linear, byte, 0);
        }
    }

    /// `len` bytes at `offset` of the shell's segment.
    pub(crate) fn get(&self, offset: u16, len: u16) -> Vec<u8> {
        (offset..offset + len)
            .map(|at| self.m.board().peek(crate::cpu::linear(self.psp, at)))
            .collect()
    }

    /// Puts `text` ASCIZ at `offset`; the offset.
    pub(crate) fn asciz(&mut self, offset: u16, text: &str) -> u16 {
        let mut bytes = Vec::from(text.as_bytes());
        bytes.push(0);
        self.put(offset, &bytes);
        offset
    }
}

/// A word of the kernel's data.
pub(crate) fn kernel16(m: &Machine, offset: u16) -> u16 {
    let at = u32::from(data::SEGMENT) * 16 + u32::from(offset);
    u16::from_le_bytes([m.board().peek(at), m.board().peek(at + 1)])
}

pub(crate) fn carry(regs: &Regs) -> bool {
    regs.flag(flag::CF)
}

pub(crate) fn ax(regs: &Regs) -> u16 {
    regs.gpr[gpr::AX]
}

/// An FCB for `name` (eight and three bytes, blank padded) on the current
/// drive, at `at`.
pub(crate) fn fcb_at(dos: &mut Dos, at: u16, name: &[u8; 11]) {
    let mut fcb = [0u8; 37];
    fcb[1..12].copy_from_slice(name);
    dos.put(at, &fcb);
}

/// FCB function `function` on the FCB at `fcb`; the result in AL.
pub(crate) fn fcb_call(dos: &mut Dos, function: u8, fcb: u16) -> u8 {
    let regs = dos.call(|r| {
        r.gpr[gpr::AX] = u16::from(function) << 8;
        r.gpr[gpr::DX] = fcb;
    });
    regs.gpr[gpr::AX].to_le_bytes()[0]
}

/// Sets AH, AL and DX to `text`'s offset.
pub(crate) fn with_path(ah: u8, al: u8, dx: u16) -> impl FnOnce(&mut Regs) {
    move |regs: &mut Regs| {
        regs.gpr[gpr::AX] = u16::from_le_bytes([al, ah]);
        regs.gpr[gpr::DX] = dx;
    }
}

/// A machine booted from `files` and the shell, its prompt showing.
pub(crate) fn shell_machine(files: &[File<'_>]) -> Machine {
    shell_machine_on(&Template::IbmPc.config(), &diskette(), files)
}

/// The same, on the machine `config` builds, from a diskette of `volume`'s
/// shape.
pub(crate) fn shell_machine_on(
    config: &crate::board::BoardConfig,
    volume: &Bpb,
    files: &[File<'_>],
) -> Machine {
    shell_in(machine_of(config, [None, None]), volume, files)
}

/// A shell that has run `AUTOEXEC.BAT` setting the prompt to `$P$G`.
pub(crate) fn prompted(files: &[File<'_>]) -> Machine {
    prompted_on(&Template::IbmPc.config(), &diskette(), files)
}

/// The same, on the machine `config` builds, from a diskette of `volume`'s
/// shape.
pub(crate) fn prompted_on(
    config: &crate::board::BoardConfig,
    volume: &Bpb,
    files: &[File<'_>],
) -> Machine {
    prompted_in(machine_of(config, [None, None]), volume, files)
}

/// The same, `m` booted.
pub(crate) fn prompted_in(m: Machine, volume: &Bpb, files: &[File<'_>]) -> Machine {
    let mut all = alloc::vec![File {
        name: b"AUTOEXECBAT",
        bytes: b"@ECHO OFF\r\nPROMPT $P$G\r\n"
    }];
    all.extend(files.iter().map(|f| File {
        name: f.name,
        bytes: f.bytes,
    }));
    shell_in(m, volume, &all)
}

/// Runs `m` until the shell waits for a key again.
pub(crate) fn settle(m: &mut Machine) {
    let begun = m.now();
    loop {
        let now = m.now();
        run_until(m, now + SECOND / 20);
        if crate::dos::waits_for_key(m) {
            return;
        }
        assert!(
            m.now() - begun < 60 * SECOND,
            "the shell never waited:\n{}",
            screen(m)
        );
    }
}

/// Types `text`, then Enter, and lets the shell run until it waits again.
pub(crate) fn typed(m: &mut Machine, text: &str) {
    type_ahead(m, &[text, "\r"].concat());
    settle(m);
}

/// Types `text`, each key a fiftieth of a second after the last, whatever
/// the shell is doing.
pub(crate) fn type_ahead(m: &mut Machine, text: &str) {
    for byte in text.bytes() {
        for &(key, down) in crate::bios::keystrokes(byte, false).as_slice() {
            let now = m.now();
            run_until(m, now + SECOND / 50);
            m.board_mut().key(key, down);
        }
    }
}

/// Presses `letter`'s key with Ctrl held, a twentieth of a second between
/// each change, whatever the shell is doing.
pub(crate) fn control(m: &mut Machine, letter: Scan) {
    const CTRL: Scan = Scan::plain(0x1D);
    for (key, down) in [(CTRL, true), (letter, true), (letter, false), (CTRL, false)] {
        let now = m.now();
        run_until(m, now + SECOND / 20);
        m.board_mut().key(key, down);
    }
}

/// The C key, for Ctrl+C.
pub(crate) const C: Scan = Scan::plain(0x2E);
