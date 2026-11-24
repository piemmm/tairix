//! FileCore's commands, each its filing system's while that is current: the
//! instance they act on is the one FileSwitch hands them in R12.

use alloc::vec::Vec;

use tairix_filecore::dir::Object;
use tairix_filecore::newmap::NewMap;
use tairix_filecore::record::NAME_LEN;
use tairix_filecore::volume::{Backing, Volume};
use tairix_filecore::{Error, Fault};
use tairix_fsmeta::preset::acorn;
use tairix_riscos_kernel::module::{Command, CommandKind};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::read_unsigned::HEXADECIMAL;

use super::disc::{op, with_volume, ClientDisc, Seen};
use super::{code, fail, located, names, own, with_instance, Client};

/// Its commands, in RISC OS 5's order.
pub(super) static COMMANDS: [Command; 13] = [
    command(
        "Backup",
        2,
        3,
        "*Backup copies the whole of one drive's disc onto another's.",
        "Syntax: *Backup <source drive> <destination drive> [Q]",
        backup,
    ),
    command(
        "Bye",
        0,
        0,
        "*Bye closes every file and dismounts every disc.",
        "Syntax: *Bye",
        bye,
    ),
    command(
        "CheckMap",
        0,
        1,
        "*CheckMap checks that a disc's map holds together.",
        "Syntax: *CheckMap [<disc>]",
        check_map,
    ),
    command(
        "Compact",
        0,
        1,
        "*Compact gathers a disc's free space together.",
        "Syntax: *Compact [<disc>]",
        compact,
    ),
    command(
        "Defect",
        2,
        2,
        "*Defect maps a faulty part of a disc out of use.",
        "Syntax: *Defect <disc> <disc address>",
        defect,
    ),
    command(
        "Dismount",
        0,
        1,
        "*Dismount closes a disc's files and forgets the disc.",
        "Syntax: *Dismount [<disc>]",
        dismount,
    ),
    command(
        "Drive",
        1,
        1,
        "*Drive sets the drive a disc of no name is on.",
        "Syntax: *Drive <drive>",
        drive,
    ),
    command(
        "Free",
        0,
        1,
        "*Free shows a disc's free and used space.",
        "Syntax: *Free [<disc>]",
        free,
    ),
    command(
        "Map",
        0,
        1,
        "*Map lists a disc's free areas.",
        "Syntax: *Map [<disc>]",
        map,
    ),
    command(
        "Mount",
        0,
        1,
        "*Mount reads a drive's disc in afresh.",
        "Syntax: *Mount [<disc>]",
        mount,
    ),
    command(
        "NameDisc",
        2,
        2,
        "*NameDisc gives a disc a new name.",
        "Syntax: *NameDisc <disc> <new name>",
        name_disc,
    ),
    command(
        "NameDisk",
        2,
        2,
        "*NameDisk gives a disc a new name.",
        "Syntax: *NameDisk <disc> <new name>",
        name_disc,
    ),
    command(
        "Verify",
        0,
        1,
        "*Verify checks that every sector of a disc reads.",
        "Syntax: *Verify [<disc>]",
        verify,
    ),
];

const fn command(
    name: &'static str,
    min: u8,
    max: u8,
    help: &'static str,
    syntax: &'static str,
    run: tairix_riscos_kernel::module::Code,
) -> Command {
    Command {
        kind: CommandKind::Filing,
        ..Command::new(name, (min, max), help, Some(syntax), run)
    }
}

/// The parameters a command was given: its tail at R0, R1 of them.
fn parameters(os: &mut Os<'_>, regs: &Regs) -> Result<Vec<Vec<u8>>, Fail> {
    let (tail, _) = os.read_until(regs.r[0], names::NAME_MOST, |b| b < b' ')?;
    let mut held = Vec::new();
    for word in tail.split(|&b| b == b' ').filter(|word| !word.is_empty()) {
        let word = tairix_util::fallible::copied(word).ok_or_else(|| os.no_memory())?;
        if held.try_reserve(1).is_err() {
            return Err(os.no_memory());
        }
        held.push(word);
    }
    Ok(held)
}

/// The instance a command acts on, by the workspace in R12, and its client.
fn instance(os: &mut Os<'_>, regs: &Regs) -> Result<(u32, Client), Fail> {
    let workspace = regs.r[12];
    let client = with_instance(os, workspace, |instance| instance.client)?;
    Ok((workspace, client))
}

/// The drive a command's disc parameter names as FileCore reads one, the
/// current drive's for none: a colon before it optional, and neither a colon
/// nor a full stop within it.
fn drive_of(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    given: Option<&Vec<u8>>,
) -> Result<u8, Fail> {
    let Some(given) = given else {
        return names::drive_of(os, workspace, None);
    };
    let disc = given.strip_prefix(b":").unwrap_or(given);
    if disc.iter().any(|&b| b == b':' || b == b'.') {
        return Err(fail(os, Some(client), code::BAD_NAME));
    }
    names::drive_of(os, workspace, Some(disc))
}

/// The drive the disc parameter of `*Free` or `*NameDisc` names, which RISC
/// OS 5 reads as FileSwitch reads a disc: a colon before it optional, a path
/// on the disc allowed after it, and a name FileSwitch would not hand on its
/// bad name.
fn drive_as_named(os: &mut Os<'_>, workspace: u32, given: Option<&Vec<u8>>) -> Result<u8, Fail> {
    let Some(given) = given else {
        return names::drive_of(os, workspace, None);
    };
    let named = given.strip_prefix(b":").unwrap_or(given);
    let mut spelt = Vec::new();
    if spelt.try_reserve_exact(named.len() + 1).is_err() {
        return Err(os.no_memory());
    }
    spelt.push(b':');
    spelt.extend_from_slice(named);
    if !crate::fileswitch::name::passes(&spelt) {
        return Err(crate::fileswitch::error::bad_name(os));
    }
    let disc = named.split(|&b| b == b'.').next().unwrap_or_default();
    names::drive_of(os, workspace, Some(disc))
}

/// `text` written on a line of its own.
fn say(os: &mut Os<'_>, token: &str, parameters: &[&[u8]]) -> Result<(), Fail> {
    let text = os.own_text(own, token, parameters)?;
    os.write_line(&text)
}

/// `*Free`: the disc's free, used and whole space.
fn free(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, _) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let drive = drive_as_named(os, workspace, given.first())?;
    let ((name, len), free, total) = with_volume(os, workspace, drive, |volume, _| {
        let space = volume.space()?;
        let unit = u64::from(space.unit);
        Ok((
            names::disc_name(volume)?,
            space.free * unit,
            space.total * unit,
        ))
    })?;
    say(os, "FreeTitle", &[&name[..len]])?;
    let decimal = |n: u64| alloc::format!("{n}").into_bytes();
    say(
        os,
        "Bytes",
        &[&decimal(free), &decimal(total - free), &decimal(total)],
    )
}

/// `*Map`: the disc's free areas, each an offset and a length.
fn map(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let drive = drive_of(os, workspace, &client, given.first())?;
    let ((name, len), areas) = with_volume(os, workspace, drive, |volume, _| {
        let mut areas = Vec::new();
        let mut short = false;
        volume.free_areas(|at, length| {
            if areas.try_reserve(1).is_ok() {
                areas.push((at, length));
            } else {
                short = true;
            }
        })?;
        if short {
            return Err(Error::Fault(tairix_filecore::Fault::NoMemory));
        }
        Ok((names::disc_name(volume)?, areas))
    })?;
    say(os, "MapHeader", &[&name[..len]])?;
    for (at, length) in areas {
        let at = alloc::format!("{at:08X}").into_bytes();
        let length = alloc::format!("{length}").into_bytes();
        say(os, "MapLine", &[&at, &length])?;
    }
    Ok(())
}

/// `*CheckMap`: the disc's map read afresh, every zone of both copies
/// checked.
fn check_map(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let drive = drive_of(os, workspace, &client, given.first())?;
    with_volume(os, workspace, drive, |volume, _| {
        if let Backing::New { map, .. } = volume.backing() {
            let record = map.record;
            NewMap::open(volume.disc_mut(), record)?;
        }
        if volume.copies_disagree() {
            return Err(Error::Fault(tairix_filecore::Fault::Broken(
                tairix_filecore::Structure::Map,
            )));
        }
        Ok(())
    })
}

/// `*Compact`: a new map's free space gathered as RISC OS 5 gathers it
/// (`Volume::compact`); an old map's files slid down into the free space
/// below each, first fit, so its free areas run together, but for a file
/// held open, whose handle reaches it where it lies.
fn compact(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let drive = drive_of(os, workspace, &client, given.first())?;
    let open = with_instance(os, workspace, |instance| {
        let mut open = Vec::new();
        let held = instance
            .files
            .iter()
            .flatten()
            .filter(|file| file.drive == drive);
        for file in held {
            open.try_reserve(1).ok()?;
            open.push(file.indaddr);
        }
        Some(open)
    })?
    .ok_or_else(|| os.no_memory())?;
    with_volume(os, workspace, drive, |volume, _| {
        if matches!(volume.backing(), Backing::New { .. }) {
            return volume.compact().map(drop);
        }
        let mut files = Vec::new();
        collect_files(volume, volume.root_addr(), &mut files, 0)?;
        files.sort_unstable_by_key(|(_, object)| object.indaddr);
        for (parent, object) in files {
            if volume.holds_nothing(object.indaddr) || open.contains(&object.indaddr) {
                continue;
            }
            let fresh = volume.allocate(u64::from(object.size))?;
            if fresh >= object.indaddr {
                volume.free(fresh, object.size)?;
                continue;
            }
            volume.copy_object(object.indaddr, fresh, u64::from(object.size))?;
            let Some((index, _)) = volume.dir_lookup(parent, object.name())? else {
                volume.free(fresh, object.size)?;
                continue;
            };
            volume.dir_update_at(
                parent,
                index,
                &Object {
                    indaddr: fresh,
                    ..object
                },
            )?;
            volume.free(object.indaddr, object.size)?;
        }
        Ok(())
    })
}

/// The deepest a compaction walks into a tree.
const DEPTH_MOST: u32 = 64;

/// Every file beneath the directory at `directory`, with the directory
/// whose entry each is.
fn collect_files(
    volume: &mut Volume<ClientDisc<'_, '_>>,
    directory: u32,
    files: &mut Vec<(u32, Object)>,
    depth: u32,
) -> Result<(), Error<Fail>> {
    if depth > DEPTH_MOST {
        return Ok(());
    }
    let handle = volume.load_dir(directory)?;
    let count = Volume::<ClientDisc<'_, '_>>::dir_len(&handle);
    let mut directories = Vec::new();
    for index in 0..count {
        let Some(object) = volume.dir_entry(&handle, directory, index)? else {
            break;
        };
        let held = if object.is_dir() {
            &mut directories
        } else {
            &mut *files
        };
        if held.try_reserve(1).is_err() {
            return Err(Error::Fault(tairix_filecore::Fault::NoMemory));
        }
        held.push((directory, object));
    }
    for (_, object) in directories {
        collect_files(volume, object.indaddr, files, depth + 1)?;
    }
    Ok(())
}

/// `*Defect`: the sector at the disc address given in hex — a sector's
/// start, within the disc — recorded as a defect as `OS_FSControl` 42
/// records one. A sector an object or the disc's own structures hold is
/// left as it is, and what holds it reported, for it to be moved first.
fn defect(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let drive = drive_of(os, workspace, &client, given.first())?;
    let Some(at) = word_start(os, regs.r[0], 1)? else {
        return Err(fail(os, Some(&client), code::BAD_PARMS));
    };
    let address = u64::from(os.number_at(at, HEXADECIMAL, 0)?.0);
    let in_use = with_volume(os, workspace, drive, |volume, _| {
        let space = volume.space()?;
        if address % volume.sector_size() != 0 || address >= u64::from(space.unit) * space.total {
            return Err(Error::Fault(Fault::BadDefect));
        }
        match volume.add_defect(address) {
            Err(Error::Fault(Fault::DefectInUse)) => Ok(true),
            added => added.map(|()| false),
        }
    })?;
    if !in_use {
        return Ok(());
    }
    let (system, _) = os.read_until(client.title, names::NAME_MOST, |b| b <= b' ')?;
    let mut name = located::Name::new(u32::MAX);
    let answer = with_volume(os, workspace, drive, |volume, _| {
        located::find(volume, &system, address, &mut name)
    })?;
    match answer {
        located::Answer::Kind(located::OBJECT) => {
            let held = name.written().split(|&b| b == 0).next().unwrap_or_default();
            say(os, "DefectObject", &[held])
        }
        _ => say(os, "DefectStructure", &[]),
    }
}

/// Where the `index`th of the words of the command tail at `tail` starts.
fn word_start(os: &mut Os<'_>, tail: u32, index: usize) -> Result<Option<u32>, Fail> {
    let (text, _) = os.read_until(tail, names::NAME_MOST, |b| b < b' ')?;
    let mut words = 0;
    let mut at = 0;
    while at < text.len() {
        if text[at] == b' ' {
            at += 1;
            continue;
        }
        if words == index {
            return Ok(Some(tail.wrapping_add(len32(at))));
        }
        words += 1;
        while text.get(at).is_some_and(|&b| b != b' ') {
            at += 1;
        }
    }
    Ok(None)
}

/// `*Dismount`: the disc's files closed and the disc forgotten.
fn dismount(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let drive = drive_of(os, workspace, &client, given.first())?;
    forget(os, workspace, Some(drive))
}

/// The files on `drive` closed and its disc forgotten; every drive's for
/// none.
fn forget(os: &mut Os<'_>, workspace: u32, drive: Option<u8>) -> Result<(), Fail> {
    let (client, busy) = with_instance(os, workspace, |instance| {
        let busy = instance
            .busy
            .iter()
            .enumerate()
            .any(|(at, &busy)| busy && drive.is_none_or(|d| usize::from(d) == at));
        (instance.client, busy)
    })?;
    if busy {
        return Err(fail(os, Some(&client), code::IN_USE));
    }
    with_instance(os, workspace, |instance| {
        for file in &mut instance.files {
            if file.is_some_and(|f| drive.is_none_or(|d| d == f.drive)) {
                *file = None;
            }
        }
        for (at, disc) in instance.discs.iter_mut().enumerate() {
            if drive.is_none_or(|d| usize::from(d) == at) {
                *disc = None;
            }
        }
    })
}

/// `*Bye`: every file closed and every disc forgotten.
fn bye(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, _) = instance(os, regs)?;
    forget(os, workspace, None)
}

/// `*Drive`: the drive a disc of no name is on — any drive it can name, one
/// the client lacks refused only when it is used, as RISC OS 5 has it.
fn drive(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let number = match given.first().map(Vec::as_slice) {
        Some(&[single]) => names::drive_named(single),
        _ => None,
    };
    let Some(number) = number else {
        return Err(fail(os, Some(&client), code::BAD_DRIVE));
    };
    with_instance(os, workspace, |instance| instance.drive = number)
}

/// `*Mount`: the drive's disc read in afresh.
fn mount(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let drive = match given.first() {
        None => with_instance(os, workspace, |instance| instance.drive)?,
        Some(given) => match given.strip_prefix(b":").unwrap_or(given) {
            &[single] => match names::drive_named(single).filter(|&drive| client.has(drive)) {
                Some(drive) => drive,
                None => return Err(fail(os, Some(&client), code::BAD_DRIVE)),
            },
            _ => return Err(fail(os, Some(&client), code::BAD_DRIVE)),
        },
    };
    forget(os, workspace, Some(drive))?;
    with_volume(os, workspace, drive, |_, _| Ok(()))
}

/// `*NameDisc`: the disc given a new name, space padded.
fn name_disc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let (Some(disc), Some(name)) = (given.first(), given.get(1)) else {
        return Err(fail(os, Some(&client), code::BAD_PARMS));
    };
    if name.is_empty() || name.len() > NAME_LEN || !name.iter().all(|&b| acorn::is_name_byte(b)) {
        return Err(fail(os, Some(&client), code::BAD_NAME));
    }
    let drive = drive_as_named(os, workspace, Some(disc))?;
    let mut padded = [b' '; NAME_LEN];
    padded[..name.len()].copy_from_slice(name);
    with_volume(os, workspace, drive, |volume, _| {
        volume.set_disc_name(padded)
    })
}

/// `*Verify`: every sector of the disc read back through its client.
fn verify(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let drive = drive_of(os, workspace, &client, given.first())?;
    with_volume(os, workspace, drive, verify_disc)?;
    say(os, "Verified", &[])
}

/// Every sector of `volume`'s disc read, a buffer at a time.
fn verify_disc(volume: &mut Volume<ClientDisc<'_, '_>>, _seen: Seen) -> Result<(), Error<Fail>> {
    let size = match volume.backing() {
        Backing::New { map, .. } => map.record.disc_size,
        Backing::Old { .. } => {
            let space = volume.space()?;
            space.total * u64::from(space.unit)
        }
    };
    let disc = volume.disc_mut();
    let step = u64::from(super::BOUNCE);
    let mut at = 0;
    while at < size {
        let length = u32::try_from((size - at).min(step)).unwrap_or(super::BOUNCE);
        let length = length - length % disc.sector();
        if length == 0 {
            break;
        }
        disc.op(op::READ, at, disc.bounce(), length)
            .map_err(Error::Disc)?;
        at += u64::from(length);
    }
    Ok(())
}

/// `*Backup`: the source drive's disc copied whole onto the destination
/// drive's, which must hold as much.
fn backup(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = instance(os, regs)?;
    let given = parameters(os, regs)?;
    let number = |given: Option<&Vec<u8>>| match given.map(Vec::as_slice) {
        Some(&[single]) => names::drive_named(single).filter(|&drive| client.has(drive)),
        _ => None,
    };
    let (Some(source), Some(destination)) = (number(given.first()), number(given.get(1))) else {
        return Err(fail(os, Some(&client), code::BAD_DRIVE));
    };
    if source == destination {
        return Err(fail(os, Some(&client), code::BAD_DRIVE));
    }
    let size = |volume: &mut Volume<ClientDisc<'_, '_>>| -> Result<u64, Error<Fail>> {
        let space = volume.space()?;
        Ok(space.total * u64::from(space.unit))
    };
    let length = with_volume(os, workspace, source, |volume, _| size(volume))?;
    let room = with_volume(os, workspace, destination, |volume, _| size(volume))?;
    if room < length {
        return Err(fail(os, Some(&client), code::DISC_FULL));
    }
    let mut at = 0;
    while at < length {
        let take =
            u32::try_from((length - at).min(u64::from(super::BOUNCE))).unwrap_or(super::BOUNCE);
        with_volume(os, workspace, source, |volume, _| {
            let disc = volume.disc_mut();
            let bounce = disc.bounce();
            disc.op(op::READ, at, bounce, take).map_err(Error::Disc)
        })?;
        with_volume(os, workspace, destination, |volume, _| {
            let disc = volume.disc_mut();
            let bounce = disc.bounce();
            disc.op(op::WRITE, at, bounce, take).map_err(Error::Disc)
        })?;
        at += u64::from(take);
    }
    forget(os, workspace, Some(destination))
}
