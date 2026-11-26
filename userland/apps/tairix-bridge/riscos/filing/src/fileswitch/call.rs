//! The calls FileSwitch makes into a filing system's entries. What each hands
//! over — a path, a special field, a block to be filled — is built below the
//! supervisor stack for the call's length.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::filing::fs;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{find, UPCALL};

use super::name::Resolved;
use super::system::{enter, info, Calls, Point};

pub use tairix_riscos_kernel::filing::fs::kind::{DIRECTORY, FILE, IMAGE, NOT_FOUND};
pub use tairix_riscos_kernel::filing::fs::Info;

/// What `FSEntry_Open` answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Opened {
    /// The file information word.
    pub info: u32,
    /// The filing system's handle, never zero.
    pub handle: u32,
    /// The buffer size, zero for an unbuffered file.
    pub buffer: u32,
    /// The extent, for a buffered file.
    pub extent: u32,
    /// The space allocated, for a buffered file.
    pub allocated: u32,
}

/// What a call names: a path, and its special field or, inside an image,
/// the image filing system's handle for the image, which R6 carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Named<'a> {
    /// The path.
    pub path: &'a [u8],
    /// The special field, if it has one.
    pub special: Option<&'a [u8]>,
    /// The image the path lies in, by the image filing system's handle.
    pub image: Option<u32>,
}

impl<'a> Named<'a> {
    /// `path`, with `special`.
    #[must_use]
    pub const fn new(path: &'a [u8], special: Option<&'a [u8]>) -> Self {
        Self {
            path,
            special,
            image: None,
        }
    }

    /// Nothing named: an empty path with no special field.
    pub const NOTHING: Named<'static> = Named {
        path: b"",
        special: None,
        image: None,
    };
}

pub use tairix_riscos_kernel::filing::fs::attribute;

/// `UpCall_ModifyingFile` for `reason` on the object `named` names, R2 and
/// R3 as given, before `calls` is asked to change it, as RISC OS 5 tells
/// whoever listens.
pub fn modifying(
    os: &mut Os<'_>,
    calls: &Calls,
    reason: u32,
    named: Named<'_>,
    r2: u32,
    r3: u32,
) -> Result<(), Fail> {
    with_names(os, calls, named, 0, |os, at, special_at, _| {
        upcall(os, calls, reason, at, special_at, r2, r3)
    })
}

/// `UpCall_ModifyingFile` for `reason` with R1 `r1`.
pub fn upcall(
    os: &mut Os<'_>,
    calls: &Calls,
    reason: u32,
    r1: u32,
    special_at: u32,
    r2: u32,
    r3: u32,
) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[0] = fs::modifying::UPCALL;
    regs.r[1] = r1;
    regs.r[2] = r2;
    regs.r[3] = r3;
    regs.r[6] = special_at;
    regs.r[8] = calls.info;
    regs.r[9] = reason;
    os.perform(tairix_riscos_kernel::swi::X | UPCALL, &mut regs)
}

/// The entry `point` of `calls`, or the error a filing system without it
/// gives.
fn target(os: &mut Os<'_>, calls: &Calls, point: Point) -> Result<super::system::Target, Fail> {
    calls
        .target(point)
        .ok_or_else(|| os.error(tairix_riscos_kernel::filing::NOT_PRESENT))
}

/// `special` refused to a filing system `calls` that takes no special field,
/// as RISC OS 5 refuses it at the call that would hand it over.
fn offer_special(os: &mut Os<'_>, calls: &Calls, special: Option<&[u8]>) -> Result<(), Fail> {
    if special.is_some() && !calls.has(info::SPECIAL_FIELDS) {
        return Err(super::error::fail(os, super::error::NO_SPECIAL_FIELDS, &[]));
    }
    Ok(())
}

/// Runs `work` with `named`'s path and special field written zero-terminated
/// below the supervisor stack, and `room` bytes after them: their addresses —
/// for an image, the image's handle in the special field's place, zero for
/// none — and the room's; a special field `calls` takes none of refused.
fn with_names<T>(
    os: &mut Os<'_>,
    calls: &Calls,
    named: Named<'_>,
    room: u32,
    work: impl FnOnce(&mut Os<'_>, u32, u32, u32) -> Result<T, Fail>,
) -> Result<T, Fail> {
    let Named {
        path,
        special,
        image,
    } = named;
    if image.is_none() {
        offer_special(os, calls, special)?;
    }
    let path_room = (len32(path.len()) + 1).next_multiple_of(4);
    let special_room = special.map_or(0, |s| (len32(s.len()) + 1).next_multiple_of(4));
    let total = path_room.saturating_add(special_room).saturating_add(room);
    os.below_supervisor_stack(total, |os, frame| {
        os.write_terminated(frame, path, 0, path.len() + 1)?;
        let special_at = match (image, special) {
            (Some(handle), _) => handle,
            (None, Some(text)) => {
                let at = frame + path_room;
                os.write_terminated(at, text, 0, text.len() + 1)?;
                at
            }
            (None, None) => 0,
        };
        work(os, frame, special_at, frame + path_room + special_room)
    })
}

/// What reading an object needs of it: its length as well, or only what it
/// is, which a filing system with `FSEntry_File` 9 is asked for alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Need {
    /// The length with the rest.
    Length,
    /// Its kind, addresses and attributes.
    Kind,
}

/// `FSEntry_File` 5, or 9 where `need` allows it and the filing system has
/// it: `path`'s catalogue information, no length read by 9.
pub fn read_info(
    os: &mut Os<'_>,
    calls: &Calls,
    named: Named<'_>,
    need: Need,
) -> Result<Info, Fail> {
    let target = target(os, calls, Point::File)?;
    let without_length = need == Need::Kind && calls.has(info::FILE_9);
    with_names(os, calls, named, 0, |os, at, special_at, _| {
        let mut regs = Regs::default();
        regs.r[0] = if without_length {
            fs::file::READ_INFO_NO_LENGTH
        } else {
            fs::file::READ_INFO
        };
        regs.r[1] = at;
        regs.r[6] = special_at;
        enter(os, target, &mut regs)?;
        let mut info = Info::answered(&regs);
        if without_length {
            info.length = 0;
        }
        Ok(info)
    })
}

/// `FSEntry_File` `reason` on `path`, R2 to R5 as `regs` gives them: R0 to
/// R5 as the filing system answers.
pub fn file(
    os: &mut Os<'_>,
    calls: &Calls,
    reason: u32,
    named: Named<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let target = target(os, calls, Point::File)?;
    with_names(os, calls, named, 0, |os, at, special_at, _| {
        regs.r[0] = reason;
        regs.r[1] = at;
        regs.r[6] = special_at;
        enter(os, target, regs)
    })
}

/// `FSEntry_File` `reason` on what `resolved` names, as [`file()`] makes it,
/// after the `UpCall_ModifyingFile` that tells of it by its whole name, R2
/// and R3 as `regs` gives them.
pub fn file_modifying(
    os: &mut Os<'_>,
    resolved: &Resolved,
    reason: u32,
    regs: &mut Regs,
) -> Result<(), Fail> {
    modifying(
        os,
        &resolved.calls,
        reason,
        resolved.whole(),
        regs.r[2],
        regs.r[3],
    )?;
    file(os, &resolved.calls, reason, resolved.named(), regs)
}

/// The `OS_Find` mode FileSwitch's own update hands a filing system in R7,
/// where a program's open hands its own, as RISC OS 5's save by open hands
/// it.
pub const OWN_UPDATE: u32 = find::UPDATE | find::MUST_OPEN;
/// The `OS_Find` mode FileSwitch's own read hands a filing system in R7.
pub const OWN_READ: u32 = find::INPUT | find::MUST_OPEN;

/// `FSEntry_Open` for update on what `resolved` names for FileSwitch's
/// `handle`, opened in `mode`, after the `UpCall_ModifyingFile` that tells
/// of it.
pub fn open_for_update(
    os: &mut Os<'_>,
    resolved: &Resolved,
    handle: u32,
    mode: u32,
) -> Result<Option<Opened>, Fail> {
    modifying(
        os,
        &resolved.calls,
        fs::modifying::OPEN,
        resolved.whole(),
        handle,
        0,
    )?;
    open(
        os,
        &resolved.calls,
        fs::open::UPDATE,
        resolved.named(),
        handle,
        mode,
    )
}

/// `FSEntry_Args` 7 ensuring `size` bytes for the file on the filing
/// system's `handle`, FileSwitch's `own`, after the `UpCall_ModifyingFile`
/// that tells of it: the size it ensured.
pub fn ensure(
    os: &mut Os<'_>,
    calls: &Calls,
    own: u32,
    handle: u32,
    size: u32,
) -> Result<u32, Fail> {
    upcall(os, calls, fs::modifying::ENSURE, own, 0, size, 0)?;
    args(os, calls, fs::args::ENSURE, handle, size, 0).map(|(got, _)| got)
}

/// What `from` names renamed to what `to` names by `FSEntry_Func` 8, after
/// the `UpCall_ModifyingFile` that tells of it, by `from`'s whole name and
/// the new name as the filing system is handed it: whether the filing system
/// did it, as R1 zero says.
pub fn rename(os: &mut Os<'_>, from: &Resolved, to: &Resolved) -> Result<bool, Fail> {
    let calls = &from.calls;
    let target = target(os, calls, Point::Func)?;
    with_names(
        os,
        calls,
        from.whole(),
        0,
        |os, whole_at, whole_special, _| {
            with_names(
                os,
                calls,
                from.named(),
                0,
                |os, from_at, from_special, _| {
                    with_names(os, calls, to.named(), 0, |os, to_at, to_special, _| {
                        upcall(
                            os,
                            calls,
                            fs::modifying::RENAME,
                            whole_at,
                            whole_special,
                            to_at,
                            0,
                        )?;
                        let mut regs = Regs::default();
                        regs.r[0] = fs::func::RENAME;
                        regs.r[1] = from_at;
                        regs.r[2] = to_at;
                        regs.r[6] = from_special;
                        regs.r[7] = to_special;
                        enter(os, target, &mut regs)?;
                        Ok(regs.r[1] == 0)
                    })
                },
            )
        },
    )
}

/// `FSEntry_Func` `reason` on `path`, with `room` bytes for it to write in
/// at R2: the registers it answers in, and the room's address.
pub fn func(
    os: &mut Os<'_>,
    calls: &Calls,
    reason: u32,
    named: Named<'_>,
    room: u32,
    regs: &mut Regs,
    read: impl FnOnce(&mut Os<'_>, &Regs, u32) -> Result<(), Fail>,
) -> Result<(), Fail> {
    let target = target(os, calls, Point::Func)?;
    with_names(os, calls, named, room, |os, at, special_at, room_at| {
        regs.r[0] = reason;
        regs.r[1] = at;
        if room > 0 {
            regs.r[2] = room_at;
        }
        regs.r[6] = special_at;
        enter(os, target, regs)?;
        read(os, regs, room_at)
    })
}

/// A special field and a disc as an image filing system's host makes them
/// canonical: none for either it answers with none.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Canonical {
    /// The special field.
    pub special: Option<Vec<u8>>,
    /// The disc.
    pub disc: Option<Vec<u8>>,
}

/// `FSEntry_Func` 23: an image filing system's host making `special` and
/// `disc` canonical, asked twice as RISC OS 5 asks it — how long each is, then
/// into buffers that long and one more, which the canonical forms are read
/// back from.
pub fn canonical_names(
    os: &mut Os<'_>,
    calls: &Calls,
    special: Option<&[u8]>,
    disc: Option<&[u8]>,
) -> Result<Canonical, Fail> {
    let target = target(os, calls, Point::Func)?;
    offer_special(os, calls, special)?;
    let room = |text: Option<&[u8]>| text.map_or(0, |t| (len32(t.len()) + 1).next_multiple_of(4));
    let (special_room, disc_room) = (room(special), room(disc));
    os.below_supervisor_stack(special_room + disc_room, |os, frame| {
        let mut given = [0; 2];
        for (slot, (text, at)) in given
            .iter_mut()
            .zip([(special, frame), (disc, frame + special_room)])
        {
            if let Some(text) = text {
                os.write_terminated(at, text, 0, text.len() + 1)?;
                *slot = at;
            }
        }
        let mut regs = Regs::default();
        regs.r[0] = fs::func::CANONICAL;
        regs.r[1..3].copy_from_slice(&given);
        enter(os, target, &mut regs)?;
        let sizes = [regs.r[3], regs.r[4]].map(|need| need.saturating_add(1));
        if sizes
            .iter()
            .any(|&size| size as usize > super::name::NAME_MOST)
        {
            return Err(super::error::bad_name(os));
        }
        let buffers = sizes.map(|size| size.next_multiple_of(4));
        os.below_supervisor_stack(buffers[0] + buffers[1], |os, at| {
            let mut regs = Regs::default();
            regs.r[0] = fs::func::CANONICAL;
            regs.r[1..3].copy_from_slice(&given);
            regs.r[3] = at;
            regs.r[4] = at + buffers[0];
            regs.r[5] = sizes[0];
            regs.r[6] = sizes[1];
            enter(os, target, &mut regs)?;
            let mut answer = [None, None];
            for (slot, (answered, (buffer, size))) in answer.iter_mut().zip(
                [regs.r[1], regs.r[2]]
                    .into_iter()
                    .zip([(at, sizes[0]), (at + buffers[0], sizes[1])]),
            ) {
                if answered != 0 {
                    *slot = Some(os.read_until(buffer, size as usize, |b| b == 0)?.0);
                }
            }
            let [special, disc] = answer;
            Ok(Canonical { special, disc })
        })
    })
}

/// `FSEntry_Func` `reason` with R1 to R7 as `regs` gives them, for a reason
/// that names nothing: R0 to R7 as the filing system answers.
pub fn func_registers(
    os: &mut Os<'_>,
    calls: &Calls,
    reason: u32,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let target = target(os, calls, Point::Func)?;
    regs.r[0] = reason;
    enter(os, target, regs)
}

/// `FSEntry_Open` `reason` on `path` for FileSwitch's `handle`, opened in
/// `mode`: none where the filing system opened nothing.
pub fn open(
    os: &mut Os<'_>,
    calls: &Calls,
    reason: u32,
    named: Named<'_>,
    handle: u32,
    mode: u32,
) -> Result<Option<Opened>, Fail> {
    let target = target(os, calls, Point::Open)?;
    with_names(os, calls, named, 0, |os, at, special_at, _| {
        let mut regs = Regs::default();
        regs.r[0] = reason;
        regs.r[1] = at;
        regs.r[2] = handle;
        regs.r[3] = handle;
        regs.r[6] = special_at;
        regs.r[7] = mode;
        enter(os, target, &mut regs)?;
        Ok((regs.r[1] != 0).then_some(Opened {
            info: regs.r[0],
            handle: regs.r[1],
            buffer: regs.r[2],
            extent: regs.r[3],
            allocated: regs.r[4],
        }))
    })
}

/// `FSEntry_GetBytes` or `FSEntry_PutBytes` for a buffered file: `count`
/// bytes between `at` and the file at `offset`.
pub fn transfer(
    os: &mut Os<'_>,
    calls: &Calls,
    point: Point,
    handle: u32,
    at: u32,
    count: u32,
    offset: u32,
) -> Result<(), Fail> {
    let target = target(os, calls, point)?;
    let mut regs = Regs::default();
    regs.r[1] = handle;
    regs.r[2] = at;
    regs.r[3] = count;
    regs.r[4] = offset;
    enter(os, target, &mut regs)
}

/// `FSEntry_GetBytes` for an unbuffered file: its next byte, none at its end.
pub fn get_byte(os: &mut Os<'_>, calls: &Calls, handle: u32) -> Result<Option<u8>, Fail> {
    let target = target(os, calls, Point::GetBytes)?;
    let mut regs = Regs::default();
    regs.r[1] = handle;
    enter(os, target, &mut regs)?;
    Ok((!regs.flag(psr::C)).then(|| regs.byte(0)))
}

/// `FSEntry_PutBytes` for an unbuffered file: `byte` put at its pointer.
pub fn put_byte(os: &mut Os<'_>, calls: &Calls, handle: u32, byte: u8) -> Result<(), Fail> {
    let target = target(os, calls, Point::PutBytes)?;
    let mut regs = Regs::default();
    regs.r[0] = u32::from(byte);
    regs.r[1] = handle;
    enter(os, target, &mut regs)
}

/// `FSEntry_Args` `reason` on `handle`, R2 and R3 as given: R2 and R3 as the
/// filing system answers.
pub fn args(
    os: &mut Os<'_>,
    calls: &Calls,
    reason: u32,
    handle: u32,
    r2: u32,
    r3: u32,
) -> Result<(u32, u32), Fail> {
    let target = target(os, calls, Point::Args)?;
    let mut regs = Regs::default();
    regs.r[0] = reason;
    regs.r[1] = handle;
    regs.r[2] = r2;
    regs.r[3] = r3;
    enter(os, target, &mut regs)?;
    Ok((regs.r[2], regs.r[3]))
}

/// `FSEntry_Close` on `handle`, stamped with `load` and `exec`, or not where
/// both are zero.
pub fn close(
    os: &mut Os<'_>,
    calls: &Calls,
    handle: u32,
    load: u32,
    exec: u32,
) -> Result<(), Fail> {
    let target = target(os, calls, Point::Close)?;
    let mut regs = Regs::default();
    regs.r[1] = handle;
    regs.r[2] = load;
    regs.r[3] = exec;
    enter(os, target, &mut regs)
}

/// `FSEntry_GBPB` `reason` on an unbuffered `handle`: R2 to R4 as the call
/// takes them in and gives them back, and C.
pub fn gbpb(
    os: &mut Os<'_>,
    calls: &Calls,
    reason: u32,
    handle: u32,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let target = target(os, calls, Point::Gbpb)?;
    regs.r[0] = reason;
    regs.r[1] = handle;
    enter(os, target, regs)
}
