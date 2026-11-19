//! A drive's disc as `lib/filecore` reaches it: whole sectors through the
//! client's `DiscOp` entry, a part sector through the instance's buffer; and
//! the mount that finds what is on a drive.

use tairix_filecore::identify::{identify, Layout};
use tairix_filecore::newmap::NewMap;
use tairix_filecore::record::{
    DiscRecord, BOOT_BLOCK_OFFSET, DISC_RECORD_IN_BOOT_BLOCK, DISC_RECORD_SIZE, LOG2SECSIZE,
};
use tairix_filecore::volume::{Backing, Mount, Volume};
use tairix_filecore::Disc;
use tairix_riscos_kernel::os::{Entry, Fail, Os, Regs};

use super::{
    boot_block_at, bounce_at, code, fail, flag, record_at, refused, with_instance, Client,
    BOOT_BLOCK_ROOM, BOUNCE, FIRST_HARD_DISC, RECORD_ROOM,
};

/// `DiscOp`'s reasons FileCore asks of a client.
pub(crate) mod op {
    /// Read sectors.
    pub(crate) const READ: u32 = 1;
    /// Write sectors.
    pub(crate) const WRITE: u32 = 2;
}

/// What R6 says when a transfer has no boot block to give.
const NO_BOOT_BLOCK: u32 = 0x8000_0000;

/// A drive's disc as FileCore holds it between operations.
pub(crate) struct Mounted {
    /// What the volume holds between operations.
    pub(crate) mount: Mount,
    /// The boot block's own copy of the record, where a boot block found the
    /// map: what RISC OS 5 reports the root's length from.
    pub(crate) boot: Option<DiscRecord>,
    /// The bytes of one sector.
    pub(crate) sector: u32,
    /// The bytes the disc holds.
    pub(crate) capacity: u64,
}

/// What an operation on a mounted disc sees of it beside its volume.
#[derive(Clone, Copy)]
pub(crate) struct Seen {
    /// The boot block's own copy of the record, where it has one.
    pub(crate) boot: Option<DiscRecord>,
}

/// `work` on the disc in `drive` of the instance whose workspace is
/// `workspace`, mounted first where it is not. The drive is held for the
/// work's length: a call into it meanwhile — from the client's own entries —
/// is refused as RISC OS refuses one, "in use".
pub(crate) fn with_volume<T>(
    os: &mut Os<'_>,
    workspace: u32,
    drive: u8,
    work: impl FnOnce(&mut Volume<ClientDisc<'_, '_>>, Seen) -> Result<T, tairix_filecore::Error<Fail>>,
) -> Result<T, Fail> {
    let slot = usize::from(drive);
    let taken = with_instance(os, workspace, |instance| {
        let busy = instance.busy.get_mut(slot)?;
        if *busy {
            return None;
        }
        *busy = true;
        Some((
            instance.client,
            instance.private_word,
            instance.discs.get_mut(slot).and_then(Option::take),
        ))
    })?;
    let Some((client, private_word, held)) = taken else {
        return Err(fail(os, None, code::IN_USE));
    };
    let mounted = match held {
        Some(mounted) => Ok(mounted),
        None => mount(os, workspace, drive),
    };
    let outcome = mounted.map(|mounted| {
        let Mounted {
            mount,
            boot,
            sector,
            capacity,
        } = mounted;
        let disc = ClientDisc::new(
            &mut *os,
            client,
            drive,
            private_word,
            workspace,
            sector,
            capacity,
        );
        let mut volume = Volume::attach(disc, mount);
        let result = work(&mut volume, Seen { boot });
        let (_, mount) = volume.detach();
        (
            Mounted {
                mount,
                boot,
                sector,
                capacity,
            },
            result,
        )
    });
    let (kept, result) = match outcome {
        Ok((mounted, result)) => (Some(mounted), Some(result)),
        Err(fail) => {
            release(os, workspace, slot, None)?;
            return Err(fail);
        }
    };
    release(os, workspace, slot, kept)?;
    match result {
        Some(Ok(done)) => Ok(done),
        Some(Err(error)) => Err(refused(os, Some(&client), error)),
        None => Err(fail(os, Some(&client), code::IN_USE)),
    }
}

/// The drive in `slot` let go, its disc kept as `kept` where it is still
/// mounted.
fn release(
    os: &mut Os<'_>,
    workspace: u32,
    slot: usize,
    kept: Option<Mounted>,
) -> Result<(), Fail> {
    with_instance(os, workspace, |instance| {
        if let Some(busy) = instance.busy.get_mut(slot) {
            *busy = false;
        }
        if let Some(disc) = instance.discs.get_mut(slot) {
            *disc = kept;
        }
    })
}

/// One drive's disc reached for one operation.
pub(crate) struct ClientDisc<'o, 'w> {
    /// The session, which the client's entries run in.
    pub(crate) os: &'o mut Os<'w>,
    client: Client,
    drive: u8,
    instance: u32,
    workspace: u32,
    sector: u32,
    capacity: u64,
}

impl<'o, 'w> ClientDisc<'o, 'w> {
    /// The disc in `drive` of the instance whose workspace is `workspace`,
    /// its private word at `instance`, of `sector`-byte sectors and
    /// `capacity` bytes.
    pub(crate) fn new(
        os: &'o mut Os<'w>,
        client: Client,
        drive: u8,
        instance: u32,
        workspace: u32,
        sector: u32,
        capacity: u64,
    ) -> Self {
        Self {
            os,
            client,
            drive,
            instance,
            workspace,
            sector,
            capacity,
        }
    }

    /// FileCore's error `code`, numbered for the client.
    pub(crate) fn fail(&mut self, code: u8) -> Fail {
        fail(self.os, Some(&self.client), code)
    }

    /// The bytes of one sector.
    pub(crate) const fn sector(&self) -> u32 {
        self.sector
    }

    /// The buffer a transfer passes through.
    pub(crate) const fn bounce(&self) -> u32 {
        bounce_at(self.workspace)
    }

    /// `DiscOp` `reason` on `length` bytes of the disc from `at`, a sector's
    /// start, to or from `ram`.
    pub(crate) fn op(&mut self, reason: u32, at: u64, ram: u32, length: u32) -> Result<(), Fail> {
        let address = if self.client.flags & flag::SECTOR_ADDRESSING != 0 {
            at / u64::from(self.sector)
        } else {
            at
        };
        let Some(address) = u32::try_from(address).ok().filter(|&a| a < 1 << 29) else {
            return Err(fail(self.os, Some(&self.client), code::DISC));
        };
        let mut regs = Regs::default();
        regs.r[1] = reason;
        regs.r[2] = u32::from(self.drive) << 29 | address;
        regs.r[3] = ram;
        regs.r[4] = length;
        regs.r[5] = record_at(self.workspace, self.drive);
        regs.r[6] = if self.drive >= FIRST_HARD_DISC {
            boot_block_at(self.workspace, self.drive)
        } else {
            NO_BOOT_BLOCK
        };
        regs.r[8] = self.instance;
        enter_client(self.os, &self.client, self.client.disc_op, &mut regs)?;
        if regs.r[4] != 0 {
            return Err(fail(self.os, Some(&self.client), code::DISC));
        }
        Ok(())
    }

    /// `length` bytes of the disc from `at` moved to or from the guest's
    /// memory at `ram`: whole sectors straight through the client, a part
    /// sector through the instance's buffer.
    pub(crate) fn transfer(
        &mut self,
        at: u64,
        ram: u32,
        length: u32,
        write: bool,
    ) -> Result<(), Fail> {
        let sector = u64::from(self.sector);
        let mut done = 0u32;
        while done < length {
            let place = at + u64::from(done);
            let within = u32::try_from(place % sector).unwrap_or(0);
            let left = length - done;
            if within == 0 && left >= self.sector {
                let whole = left - left % self.sector;
                let reason = if write { op::WRITE } else { op::READ };
                self.op(reason, place, ram.wrapping_add(done), whole)?;
                done += whole;
                continue;
            }
            let start = place - u64::from(within);
            let take = left.min(self.sector - within);
            let bounce = self.bounce();
            self.op(op::READ, start, bounce, self.sector)?;
            let guest = ram.wrapping_add(done);
            if write {
                self.os.copy_bytes(guest, bounce + within, take)?;
                self.op(op::WRITE, start, bounce, self.sector)?;
            } else {
                self.os.copy_bytes(bounce + within, guest, take)?;
            }
            done += take;
        }
        Ok(())
    }

    /// The part of `wanted` bytes from `at` one pass through the buffer
    /// takes.
    fn span(&self, at: u64, wanted: u64) -> Span {
        let within = u32::try_from(at % u64::from(self.sector)).unwrap_or(0);
        let take = u32::try_from(wanted)
            .unwrap_or(u32::MAX)
            .min(BOUNCE - within);
        Span {
            start: at - u64::from(within),
            within,
            take,
            length: (within + take).next_multiple_of(self.sector),
        }
    }

    /// The sectors `span` only partly covers read into the buffer, so a write
    /// of it leaves the rest of them as the disc holds them.
    fn keep_edges(&mut self, span: &Span) -> Result<(), Fail> {
        let bounce = self.bounce();
        let head = span.within != 0;
        if head {
            self.op(op::READ, span.start, bounce, self.sector)?;
        }
        let last = span.length - self.sector;
        let tail_unread = !head || last != 0;
        if tail_unread && !(span.within + span.take).is_multiple_of(self.sector) {
            self.op(
                op::READ,
                span.start + u64::from(last),
                bounce + last,
                self.sector,
            )?;
        }
        Ok(())
    }

    /// `span`, its new bytes put in the buffer by `fill`, written.
    fn write_span(
        &mut self,
        span: &Span,
        fill: impl FnOnce(&mut Os<'_>, u32) -> Result<(), Fail>,
    ) -> Result<(), Fail> {
        self.keep_edges(span)?;
        let bounce = self.bounce();
        fill(self.os, bounce + span.within)?;
        self.op(op::WRITE, span.start, bounce, span.length)
    }
}

/// One pass of a transfer through the buffer: whole sectors of the disc from
/// `start`, `length` bytes, holding `take` bytes from `within` the first.
struct Span {
    start: u64,
    within: u32,
    take: u32,
    length: u32,
}

impl Disc for ClientDisc<'_, '_> {
    type Error = Fail;

    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> Result<(), Fail> {
        let mut done = 0;
        while done < out.len() {
            let span = self.span(at + done as u64, (out.len() - done) as u64);
            let bounce = self.bounce();
            self.op(op::READ, span.start, bounce, span.length)?;
            let take = span.take as usize;
            self.os
                .read_bytes(bounce + span.within, &mut out[done..done + take])?;
            done += take;
        }
        Ok(())
    }

    fn write(&mut self, at: u64, bytes: &[u8]) -> Result<(), Fail> {
        let mut done = 0;
        while done < bytes.len() {
            let span = self.span(at + done as u64, (bytes.len() - done) as u64);
            let take = span.take as usize;
            let piece = &bytes[done..done + take];
            self.write_span(&span, |os, into| os.write_bytes(into, piece))?;
            done += take;
        }
        Ok(())
    }

    fn zero(&mut self, at: u64, length: u64) -> Result<(), Fail> {
        let mut done = 0;
        while done < length {
            let span = self.span(at + done, length - done);
            let take = span.take;
            self.write_span(&span, |os, into| os.fill(into, take, 0))?;
            done += u64::from(take);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Fail> {
        Ok(())
    }
}

/// The client's entry `target` called with `regs`, its private word in R12;
/// an error it gives decoded as FileCore's entries give them: a block at
/// R0 less bit 30, a disc error with bit 31, else a number in its low byte.
pub(crate) fn enter_client(
    os: &mut Os<'_>,
    client: &Client,
    target: tairix_riscos_kernel::os::Target,
    regs: &mut Regs,
) -> Result<(), Fail> {
    regs.r[12] = client.private_word;
    match os.call_routine(target.address, target.bits, Entry::Supervisor, regs) {
        Ok(()) => Ok(()),
        Err(Fail::Error(word)) if word & 1 << 30 != 0 => Err(Fail::Error(word & !(1 << 30))),
        Err(Fail::Error(word)) if word & 1 << 31 != 0 => Err(fail(os, Some(client), code::DISC)),
        Err(Fail::Error(word)) => Err(fail(os, Some(client), word.to_le_bytes()[0])),
        Err(unwind) => Err(unwind),
    }
}

/// The disc in `drive` of the instance whose workspace is `workspace`,
/// mounted: its client asked to mount it, which fills in its geometry, and
/// its structures found as `FileCore` finds them.
pub(crate) fn mount(os: &mut Os<'_>, workspace: u32, drive: u8) -> Result<Mounted, Fail> {
    let (client, instance) = with_instance(os, workspace, |i| (i.client, i.private_word))?;
    if !client.has(drive) {
        return Err(fail(os, Some(&client), code::BAD_DRIVE));
    }
    let record_at = record_at(workspace, drive);
    os.fill(record_at, RECORD_ROOM, 0)?;
    let mut regs = Regs::default();
    regs.r[1] = u32::from(drive);
    regs.r[2] = if drive >= FIRST_HARD_DISC {
        u32::try_from(BOOT_BLOCK_OFFSET).unwrap_or(0)
    } else {
        0
    };
    regs.r[3] = bounce_at(workspace);
    regs.r[4] = BOUNCE;
    regs.r[5] = record_at;
    enter_client(os, &client, client.misc_op, &mut regs)?;
    let hard_disc = drive >= FIRST_HARD_DISC;
    let mut raw = [0u8; DISC_RECORD_SIZE];
    if hard_disc {
        // A hard disc's mount reads its boot block, which its client's later
        // transfers are given; one whose client fills no geometry is
        // described by the boot block's own record.
        let mut block = [0u8; BOOT_BLOCK_ROOM as usize];
        os.read_bytes(bounce_at(workspace), &mut block)?;
        os.write_bytes(boot_block_at(workspace, drive), &block)?;
        if client.flags & flag::HARD_DISC_MOUNT == 0 {
            raw.copy_from_slice(
                &block[DISC_RECORD_IN_BOOT_BLOCK..DISC_RECORD_IN_BOOT_BLOCK + DISC_RECORD_SIZE],
            );
            os.write_bytes(record_at, &raw)?;
        }
    }
    if !hard_disc || client.flags & flag::HARD_DISC_MOUNT != 0 {
        os.read_bytes(record_at, &mut raw)?;
    }
    let given = DiscRecord::parse(&raw);
    if !LOG2SECSIZE.contains(&given.log2secsize) {
        return Err(fail(os, Some(&client), code::BAD_DISC));
    }
    let capacity = if given.disc_size == 0 {
        u64::MAX
    } else {
        given.disc_size
    };
    let sector = 1u32 << given.log2secsize;
    let mut disc = ClientDisc::new(
        &mut *os, client, drive, instance, workspace, sector, capacity,
    );
    let found = match identify(&mut disc) {
        Ok(Some(found)) => found,
        Ok(None) => return Err(fail(disc.os, Some(&client), code::DISC_NOT_FILE_CORE)),
        Err(error) => return Err(refused(disc.os, Some(&client), error)),
    };
    let (backing, boot, record) = match found {
        Layout::NewMap { record, boot } => match NewMap::open(&mut disc, record) {
            Ok(map) => {
                let record = map.record;
                (
                    Backing::New {
                        map,
                        boot_block: boot.is_some(),
                    },
                    boot,
                    record,
                )
            }
            Err(error) => return Err(refused(disc.os, Some(&client), error)),
        },
        Layout::OldMap {
            large_directories,
            root_sector,
            ..
        } => (
            Backing::Old {
                format: if large_directories {
                    tairix_filecore::dir::FixedFormat::New
                } else {
                    tairix_filecore::dir::FixedFormat::Old
                },
                root_sector,
            },
            None,
            given,
        ),
    };
    disc.os.write_bytes(record_at, &record.encode())?;
    let opened = Volume::open(disc, backing).map(|volume| volume.detach().1);
    match opened {
        Ok(mount) => Ok(Mounted {
            mount,
            boot,
            sector,
            capacity,
        }),
        Err(error) => Err(refused(os, Some(&client), error)),
    }
}
