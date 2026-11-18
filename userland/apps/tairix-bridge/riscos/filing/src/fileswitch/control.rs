//! `OS_FSControl` (`plans/RISCOS-EMULATOR.md` §6): the filing systems
//! registered, selected and removed, prefixes and canonical names, and each
//! filing system's directories.
//!
//! A directory is kept as the path it was set to, without its filing
//! system's prefix, beside the special field it was set with. Setting the
//! current directory makes the one before it the previous directory.

use alloc::vec::Vec;

use tairix_riscos_kernel::filing::fs;
use tairix_riscos_kernel::filing::{BAD_FS_CONTROL, NOT_PRESENT};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::fs_control as reason;

use super::call::{self, Need, DIRECTORY, NOT_FOUND as NOTHING};
use super::listing::{self, Detail};
use super::name::{self, Dir, Directories, Prefix, Search, Selection};
use super::system::{self as systems, Refused, System};
use super::{args, error, image, open, run, tree, types, with, State};

/// `*Opt` 0, every option reset, and 1, FileSwitch's own.
const OPT_RESET: u32 = 0;
const OPT_MESSAGES: u32 = 1;

/// `FSCV`'s default.
pub(crate) fn fs_control(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        reason::SET_CURRENT => set_from_name(os, regs.r[1], Dir::Current),
        reason::SET_LIBRARY => set_from_name(os, regs.r[1], Dir::Library),
        reason::SET_USER_ROOT => set_from_name(os, regs.r[1], Dir::UserRoot),
        reason::START_APPLICATION => os.start_new_application(regs),
        reason::RUN => run::run(os, regs),
        reason::RESERVED => reserved(os, regs),
        reason::CAT => listing::catalogue(os, regs, false, Detail::Names),
        reason::EX => listing::catalogue(os, regs, false, Detail::Information),
        reason::LIBRARY_CAT => listing::catalogue(os, regs, true, Detail::Names),
        reason::LIBRARY_EX => listing::catalogue(os, regs, true, Detail::Information),
        reason::INFO => listing::info(os, regs, Detail::Information),
        reason::FILE_INFO => listing::info(os, regs, Detail::Full),
        reason::OPT => opt(os, regs),
        reason::BOOT => boot(os),
        reason::PREFIX => prefix(os, regs),
        reason::ADD => add(os, regs),
        reason::PRESENT => present(os, regs),
        reason::SELECT => select(os, regs.r[1]),
        reason::REMOVE => remove(os, regs.r[1]),
        reason::ADD_SECONDARY => add_secondary(os, regs),
        reason::TYPE_NAME => types::type_name(os, regs),
        reason::RESTORE => with(os, |state| state.prefixed = None),
        reason::TEMPORARY_MODULE | reason::SECONDARY_MODULE => module_of(os, regs),
        reason::HANDLE => handle(os, regs),
        reason::CLOSE_ALL => open::close_all(os, None),
        reason::SHUT_DOWN => shut_down(os),
        reason::ACCESS => tree::access(os, regs),
        reason::RENAME => tree::rename(os, regs),
        reason::COPY => tree::copy(os, regs),
        reason::WIPE => tree::wipe(os, regs),
        reason::COUNT => tree::count(os, regs),
        reason::TYPE_NUMBER => types::type_number(os, regs),
        reason::NUMBER_TO_NAME => number_to_name(os, regs),
        reason::ADD_IMAGE_SYSTEM => image::register(os, regs),
        reason::REMOVE_IMAGE_SYSTEM => image::remove(os, regs.r[1] & 0xFFF),
        reason::CANONICALISE => canonicalise(os, regs),
        reason::TYPE_OF_INFO => types::type_of_info(os, regs),
        reason::DEFECT_LIST..=reason::OBJECT_AT_OFFSET
        | reason::FREE_SPACE_64..=reason::ADD_DEFECT_64
            if disc_entry(regs.r[0]).is_some() =>
        {
            disc_operation(os, regs)
        }
        reason::NEXT_OPEN => next_open(os, regs),
        reason::BACK => back(os),
        reason::UNSET_CURRENT => unset(os, Dir::Current),
        reason::UNSET_USER_ROOT => unset(os, Dir::UserRoot),
        reason::UNSET_LIBRARY => unset(os, Dir::Library),
        reason::SET_UNCHECKED => set_unchecked(os, regs),
        reason::READ_PATH => read_path(os, regs),
        _ => Err(os.error(BAD_FS_CONTROL)),
    }
}

/// The filing system the text at `at` names, as `OS_FSControl` 13 and 14
/// take one: a number below 256, else a name.
fn named_system(os: &mut Os<'_>, at: u32, any_end: bool) -> Result<Option<u8>, Fail> {
    if let Ok(number) = u8::try_from(at) {
        return with(os, |state| state.system(number).map(|s| s.number));
    }
    let (name, _) = os.read_until(at, systems::NAME_MOST + 1, |b| {
        b < b' ' || (any_end && matches!(b, b'#' | b':' | b'-'))
    })?;
    if name.is_empty()
        || name.len() > systems::NAME_MOST
        || !name.iter().all(|&b| systems::name_byte(b))
    {
        return Err(error::no_prefix(os, &name));
    }
    with(os, |state| {
        state
            .systems
            .iter()
            .find(|s| s.named(&name))
            .map(|s| s.number)
    })
}

/// `OS_FSControl` 12: the filing system whose information block is R2 past
/// its module at R1 registered, R3 its entries' R12; one registered again
/// takes its old place, as RISC OS 5's re-registration does.
fn add(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let bits = os.caller_bits();
    let system = match System::read(os, regs.r[1], regs.r[2], regs.r[3], bits)? {
        Ok(system) => system,
        Err(Refused::BadName | Refused::NoNumber) => return Err(error::bad_name(os)),
    };
    let (number, block) = (system.number, system.block);
    let placed = with(os, |state| {
        let old = state
            .systems
            .iter()
            .position(|s| s.number == number || s.named(&system.name));
        let replaced = if let Some(slot) = old.and_then(|place| state.systems.get_mut(place)) {
            Some(core::mem::replace(slot, system).block)
        } else {
            if state.systems.try_reserve(1).is_err() {
                return Err(());
            }
            state.systems.push(system);
            None
        };
        if state.current.is_none() && state.removed_current == Some(number) {
            state.current = Some(number);
            state.prefixed = None;
            state.removed_current = None;
        }
        Ok(replaced)
    })?;
    match placed {
        Ok(Some(old)) => os.rma_free(old),
        Ok(None) => Ok(()),
        Err(()) => {
            os.rma_free(block)?;
            Err(os.no_memory())
        }
    }
}

/// `OS_FSControl` 13: R1 the filing system's number and R2 FileSwitch's
/// block for it, R2 zero for none.
fn present(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let number = named_system(os, regs.r[1], regs.r[2] == 0)?;
    let block = match number {
        Some(number) => with(os, |state| state.system(number).map_or(0, |s| s.block))?,
        None => 0,
    };
    if let Some(number) = number {
        regs.r[1] = u32::from(number);
    }
    regs.r[2] = block;
    Ok(())
}

/// `OS_FSControl` 14: the filing system R1 names made current and
/// temporary, or none for zero.
fn select(os: &mut Os<'_>, at: u32) -> Result<(), Fail> {
    if at == 0 {
        return with(os, |state| {
            state.current = None;
            state.prefixed = None;
        });
    }
    match named_system(os, at, false)? {
        Some(number) => with(os, |state| {
            state.current = Some(number);
            state.prefixed = None;
            state.removed_current = None;
        }),
        None => Err(no_system(os, at)),
    }
}

/// "Filing system … not present", naming the name at `at`.
fn no_system(os: &mut Os<'_>, at: u32) -> Fail {
    if u8::try_from(at).is_ok() {
        return os.error(NOT_PRESENT);
    }
    match os.read_until(at, systems::NAME_MOST, |b| b < b' ') {
        Ok((name, _)) => error::no_prefix(os, &name),
        Err(fail) => fail,
    }
}

/// `OS_FSControl` 16: the filing system R1 names removed, its open files
/// closed and its copy of the block freed. A current one is remembered, and
/// current again should it come back before another is selected.
fn remove(os: &mut Os<'_>, at: u32) -> Result<(), Fail> {
    let Some(number) = named_system(os, at, false)? else {
        return Err(no_system(os, at));
    };
    open::close_all(os, Some(number))?;
    let gone = with(os, |state| {
        let place = state.systems.iter().position(|s| s.number == number)?;
        let system = state.systems.remove(place);
        if state.current == Some(number) {
            state.current = None;
            state.prefixed = None;
            state.removed_current = Some(number);
        } else if state.prefixed == Some(number) {
            state.prefixed = None;
        }
        Some(system.block)
    })?;
    if let Some(block) = gone {
        os.rma_free(block)?;
    }
    Ok(())
}

/// `OS_FSControl` 17: the secondary module whose name is at R2 added to the
/// filing system R1 names, R3 its private word; one added again replaces it.
fn add_secondary(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let Some(number) = named_system(os, regs.r[1], false)? else {
        return Err(no_system(os, regs.r[1]));
    };
    let secondary = (regs.r[2], regs.r[3]);
    with(os, |state| {
        if let Some(system) = state.systems.iter_mut().find(|s| s.number == number) {
            system.secondary = Some(secondary);
        }
    })
}

/// `OS_FSControl` 20 and 30: the temporary filing system's module and its
/// R12, or where its secondary module's name was given and its private word;
/// zeros for none.
fn module_of(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let secondary = regs.r[0] == reason::SECONDARY_MODULE;
    let found = with(os, |state| {
        let system = state.system(state.temporary()?)?;
        if secondary {
            system.secondary
        } else {
            Some((system.base, system.workspace))
        }
    })?;
    let (r1, r2) = found.unwrap_or((0, 0));
    regs.r[1] = r1;
    regs.r[2] = r2;
    Ok(())
}

/// `OS_FSControl` 21: the filing system's handle and information word for
/// the file on R1, zeros where none is open on it.
fn handle(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let stream = with(os, |state| state.files.stream(regs.r[1]))?;
    let (r1, r2) = stream.map_or((0, 0), |s| (s.handle, s.calls.info));
    regs.r[1] = r1;
    regs.r[2] = r2;
    Ok(())
}

/// `OS_FSControl` 3, which RISC OS 5 refuses once it has read the object R1
/// names.
fn reserved(os: &mut Os<'_>, regs: &Regs) -> Result<(), Fail> {
    name::find(os, regs.r[1], Search::Plain, Need::Length)?;
    Err(error::fail(os, error::RESERVED, &[]))
}

/// The `FSEntry_Func` reason an image filing system or its host takes for a
/// disc's `OS_FSControl` reason, and which of R0 to R4 it answers in.
fn disc_entry(reason: u32) -> Option<(u32, core::ops::Range<usize>)> {
    Some(match reason {
        reason::DEFECT_LIST => (25, 0..0),
        reason::ADD_DEFECT => (26, 0..0),
        reason::READ_BOOT_OPTION => (27, 2..3),
        reason::WRITE_BOOT_OPTION => (28, 0..0),
        reason::USED_SPACE_MAP => (29, 0..0),
        reason::FREE_SPACE => (30, 0..3),
        reason::NAME_DISC => (31, 0..0),
        reason::STAMP_IMAGE => (32, 0..0),
        reason::OBJECT_AT_OFFSET => (33, 2..3),
        reason::FREE_SPACE_64 => (35, 0..5),
        reason::DEFECT_LIST_64 => (36, 0..0),
        reason::ADD_DEFECT_64 => (37, 0..0),
        _ => return None,
    })
}

/// `FSEntry_Func` 7, the boot option's: `*Opt 4`.
const OPT_BOOT: u32 = 4;

/// `OS_FSControl` 41, 42, 46 to 52 and 55 to 57: an operation on the disc or
/// image holding the object R1 names, passed with R2 to R5 to its image
/// filing system or host. Any other filing system can read and write only
/// its disc's boot option, through `FSEntry_Func` 0 selecting the disc's
/// root and then 11 or 7, as RISC OS 5 asks it; the rest it does not
/// support.
fn disc_operation(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let Some((entry, answers)) = disc_entry(regs.r[0]) else {
        return Err(os.error(BAD_FS_CONTROL));
    };
    let (mut resolved, found) = name::find_on_disc(os, regs.r[1])?;
    if found.kind == NOTHING {
        let given = name::read(os, regs.r[1])?;
        return Err(name::named_error(
            os,
            tairix_riscos_kernel::filing::NOT_FOUND,
            &given,
        ));
    }
    image::root(os, &mut resolved, &found)?;
    if resolved.inside.is_none() && !resolved.calls.has(systems::info::IMAGES) {
        return plain_disc_operation(os, regs, &resolved);
    }
    let mut call = Regs::default();
    call.r[2..6].copy_from_slice(&regs.r[2..6]);
    call::func(
        os,
        &resolved.calls,
        entry,
        resolved.named(),
        0,
        &mut call,
        |_, _, _| Ok(()),
    )?;
    regs.r[answers.clone()].copy_from_slice(&call.r[answers]);
    Ok(())
}

/// A disc's operation on a filing system that is no image filing system's
/// host.
fn plain_disc_operation(
    os: &mut Os<'_>,
    regs: &mut Regs,
    resolved: &name::Resolved,
) -> Result<(), Fail> {
    let root = name::disc_root_of(&resolved.path).to_vec();
    let selector = call::Named::new(&root, resolved.special());
    match regs.r[0] {
        reason::READ_BOOT_OPTION => {
            let (_, boot) = name::read_disc(os, &resolved.calls, selector)?;
            regs.r[2] = u32::from(boot);
            Ok(())
        }
        reason::WRITE_BOOT_OPTION => {
            let mut select = Regs::default();
            call::func(
                os,
                &resolved.calls,
                fs::func::SET_DIRECTORY,
                selector,
                0,
                &mut select,
                |_, _, _| Ok(()),
            )?;
            let mut option = Regs::default();
            option.r[1] = OPT_BOOT;
            option.r[2] = regs.r[2];
            call::func_registers(os, &resolved.calls, fs::func::OPT, &mut option)
        }
        _ => Err(os.error(tairix_riscos_kernel::filing::UNSUPPORTED)),
    }
}

/// `OS_FSControl` 58: the open file after the handle in R1 — from the first
/// for a handle of zero or below — R1 its handle, R0 its stream status and R2
/// its filing system's information word; R1 -1 after the last.
fn next_open(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let after = i64::from(regs.r[1].cast_signed()).max(0);
    let found = with(os, |state| {
        (1..=open::HANDLES)
            .filter(|&handle| i64::from(handle) > after)
            .find_map(|handle| state.files.stream(handle).map(|stream| (handle, stream)))
    })?;
    match found {
        Some((handle, stream)) => {
            regs.r[0] = args::status_of(&stream);
            regs.r[1] = handle;
            regs.r[2] = stream.calls.info;
        }
        None => regs.r[1] = u32::MAX,
    }
    Ok(())
}

/// What a call into the current filing system needs, or "no filing system".
fn current_calls(os: &mut Os<'_>) -> Result<systems::Calls, Fail> {
    let calls = with(os, |state| {
        state
            .current
            .and_then(|n| state.system(n))
            .map(System::calls)
    })?;
    calls.ok_or_else(|| os.error(NOT_PRESENT))
}

/// `OS_FSControl` 10: option R1 of the temporary filing system set to R2 —
/// option 1, how much a load, save or creation tells, FileSwitch's own;
/// any other the filing system's, by its `FSEntry_Func` 7. Option 0 resets
/// every option, FileSwitch's too.
fn opt(os: &mut Os<'_>, regs: &Regs) -> Result<(), Fail> {
    let number = with(os, |state| state.temporary())?;
    let Some(number) = number else {
        return Err(os.error(NOT_PRESENT));
    };
    let calls = with(os, |state| {
        let system = state.systems.iter_mut().find(|s| s.number == number)?;
        match regs.r[1] {
            OPT_MESSAGES => system.messages = regs.byte(2) & 3,
            OPT_RESET => system.messages = 0,
            _ => {}
        }
        Some(system.calls())
    })?;
    let Some(calls) = calls else {
        return Err(os.error(NOT_PRESENT));
    };
    if regs.r[1] == OPT_MESSAGES {
        return Ok(());
    }
    let mut call = Regs::default();
    call.r[1] = regs.r[1];
    call.r[2] = regs.r[2];
    call::func_registers(os, &calls, fs::func::OPT, &mut call)
}

/// `OS_FSControl` 15: the current filing system booted, by its
/// `FSEntry_Func` 10.
fn boot(os: &mut Os<'_>) -> Result<(), Fail> {
    let calls = current_calls(os)?;
    call::func_registers(os, &calls, fs::func::BOOT, &mut Regs::default())
}

/// `OS_FSControl` 23: every file closed and every filing system told it is
/// being shut down.
fn shut_down(os: &mut Os<'_>) -> Result<(), Fail> {
    open::close_all(os, None)?;
    let all: Vec<_> = with(os, |state| {
        state.systems.iter().map(System::calls).collect()
    })?;
    for calls in all {
        call::func_registers(os, &calls, fs::func::SHUT_DOWN, &mut Regs::default())?;
    }
    Ok(())
}

/// `OS_FSControl` 33: the name of the filing system numbered R1 into R2's
/// R3 bytes, empty for one not registered.
fn number_to_name(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = name_of_number(os, regs.r[1])?.unwrap_or_default();
    os.write_terminated(regs.r[2], &name, 0, regs.r[3] as usize)
}

/// The name of the filing system numbered `number`; none for one not
/// registered, nought among them.
pub(crate) fn name_of_number(os: &mut Os<'_>, number: u32) -> Result<Option<Vec<u8>>, Fail> {
    with(os, |state| {
        let number = u8::try_from(number).ok()?;
        state.system(number).map(|s| s.name.clone())
    })
}

/// `OS_FSControl` 11: the temporary filing system set from the prefix at
/// R1; R1 past it, R2 its number or -1 for none, R3 its special field or 0.
fn prefix(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (text, start) = name::read_from(os, regs.r[1])?;
    let parsed = name::parse_taking(os, &text, name::Unknown::Plain)?;
    regs.r[3] = match (&parsed.special, text.iter().position(|&b| b == b'#')) {
        (Some(special), Some(hash)) if !special.is_empty() => start.wrapping_add(len32(hash) + 1),
        _ => 0,
    };
    match parsed.prefix {
        Prefix::System(number) => {
            with(os, |state| state.prefixed = Some(number))?;
            regs.r[1] = start.wrapping_add(len32(text.len() - parsed.path.len()));
            regs.r[2] = u32::from(number);
        }
        Prefix::Current | Prefix::Variable(_) => regs.r[2] = u32::MAX,
    }
    Ok(())
}

/// `OS_FSControl` 37: the name at R1 made canonical — its filing system,
/// special field, disc and directories filled in and its wildcards matched —
/// through R3's path variable or R4's path, into R2's R5 bytes; R5 what is
/// spare after the name, the terminator's byte among it.
fn canonicalise(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let path_text;
    let search = if regs.r[3] != 0 {
        path_text = name::read(os, regs.r[3])?;
        Search::Variable(&path_text)
    } else if regs.r[4] != 0 {
        path_text = name::read(os, regs.r[4])?;
        Search::Path(&path_text)
    } else {
        Search::Plain
    };
    let resolved = name::canonical(os, regs.r[1], search)?;
    let text = args::canonical(os, resolved.calls.number, resolved.whole())?;
    args::give(os, &text, regs.r[2], regs, args::Spare::WithTerminator).map(drop)
}

/// The directories of the filing system `number`, made empty where it has
/// none yet.
fn directories(state: &mut State, number: u8) -> Option<&mut Directories> {
    let place = if let Some(place) = state.directories.iter().position(|(n, _)| *n == number) {
        place
    } else {
        state.directories.try_reserve(1).ok()?;
        state.directories.push((number, Directories::default()));
        state.directories.len() - 1
    };
    Some(&mut state.directories.get_mut(place)?.1)
}

/// The directories of the filing system `number`, changed by `change`.
fn change(os: &mut Os<'_>, number: u8, change: impl FnOnce(&mut Directories)) -> Result<(), Fail> {
    let done = with(os, |state| directories(state, number).map(change))?;
    done.ok_or_else(|| os.no_memory())
}

/// `OS_FSControl` 0, 1 and 39: the directory R1 names, which must be one,
/// set as `which` of its filing system, which a prefix also selects. No name
/// at all, a control character first, names `&` — for the user root, `$`.
fn set_from_name(os: &mut Os<'_>, at: u32, which: Dir) -> Result<(), Fail> {
    let text = if at != 0 && os.read_byte(at)? < b' ' {
        let unnamed: &[u8] = if which == Dir::UserRoot { b"$" } else { b"&" };
        unnamed.to_vec()
    } else {
        name::read(os, at)?
    };
    let (resolved, found) = name::find_named(os, &text, Search::Plain, Need::Kind)?;
    if found.kind != DIRECTORY && found.kind != call::IMAGE {
        let error = if found.kind == NOTHING {
            tairix_riscos_kernel::filing::NO_DIRECTORY
        } else {
            tairix_riscos_kernel::filing::IS_A_FILE
        };
        return Err(name::named_error(os, error, &text));
    }
    let number = resolved.calls.number;
    let selection = Selection {
        path: resolved.path,
        special: resolved.special,
    };
    change(os, number, |directories| {
        if which == Dir::Current {
            directories.enter(selection);
        } else {
            directories.put(which, Some(selection));
        }
    })?;
    if let Prefix::System(chosen) = name::parse(os, &text)?.prefix {
        with(os, |state| {
            state.current = Some(chosen);
            state.prefixed = None;
        })?;
    }
    Ok(())
}

/// The temporary filing system's number, or "no filing system".
fn temporary(os: &mut Os<'_>) -> Result<u8, Fail> {
    match with(os, |state| state.temporary())? {
        Some(number) => Ok(number),
        None => Err(os.error(NOT_PRESENT)),
    }
}

/// `OS_FSControl` 40: the temporary filing system's current and previous
/// directories exchanged, the previous one read first as RISC OS 5 reads it,
/// since it must still be a directory.
fn back(os: &mut Os<'_>) -> Result<(), Fail> {
    let number = temporary(os)?;
    let (_, found) = name::find_named(os, b"\\", Search::Plain, Need::Kind)?;
    if found.kind != DIRECTORY && found.kind != call::IMAGE {
        return Err(name::named_error(
            os,
            tairix_riscos_kernel::filing::NO_DIRECTORY,
            b"\\",
        ));
    }
    change(os, number, |directories| {
        directories.exchange(Dir::Current, Dir::Previous);
    })
}

/// `OS_FSControl` 43 to 45: `which` of the temporary filing system unset;
/// the previous directory stays as it was, as RISC OS 5 leaves it.
fn unset(os: &mut Os<'_>, which: Dir) -> Result<(), Fail> {
    let number = temporary(os)?;
    change(os, number, |directories| directories.put(which, None))
}

/// The directory a register numbers.
fn dir_numbered(os: &mut Os<'_>, number: u32) -> Result<Dir, Fail> {
    match number {
        0 => Ok(Dir::Current),
        1 => Ok(Dir::Previous),
        2 => Ok(Dir::UserRoot),
        3 => Ok(Dir::Library),
        _ => Err(os.error(tairix_riscos_kernel::error::BAD_PARAMETERS)),
    }
}

/// `OS_FSControl` 53: directory R2 of the filing system R3 names set to the
/// path at R1 as it is, unchecked, with R6's special field; the previous
/// directory is left as it was.
fn set_unchecked(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let which = dir_numbered(os, regs.r[2])?;
    let Some(number) = named_system(os, regs.r[3], false)? else {
        return Err(no_system(os, regs.r[3]));
    };
    let path = name::read(os, regs.r[1])?;
    let special = if regs.r[6] == 0 {
        None
    } else {
        let (special, _) = os.read_until(regs.r[6], name::NAME_MOST, |b| b < b' ' || b == b',')?;
        Some(special)
    };
    change(os, number, |directories| {
        directories.put(which, Some(Selection { path, special }));
    })
}

/// `OS_FSControl` 54: directory R2 of the filing system R3 names, as it was
/// set, into R1's R5 bytes — `#`, its special field and a terminator before
/// its path where it has one; R1 the path, zero where it is not set, and R6
/// the special field or zero.
fn read_path(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let which = dir_numbered(os, regs.r[2])?;
    let Some(number) = named_system(os, regs.r[3], false)? else {
        return Err(no_system(os, regs.r[3]));
    };
    let set = with(os, |state| {
        state
            .directories
            .iter()
            .find(|(n, _)| *n == number)
            .and_then(|(_, d)| d.get(which).cloned())
    })?;
    let Some(selection) = set else {
        regs.r[1] = 0;
        regs.r[6] = 0;
        return Ok(());
    };
    let buffer = regs.r[1];
    let before = selection.special.as_ref().map_or(0, |s| s.len() + 2);
    let mut text = Vec::new();
    if text
        .try_reserve_exact(before + selection.path.len())
        .is_err()
    {
        return Err(os.no_memory());
    }
    if let Some(special) = &selection.special {
        text.push(b'#');
        text.extend_from_slice(special);
        text.push(0);
    }
    text.extend_from_slice(&selection.path);
    let fits = args::give(os, &text, buffer, regs, args::Spare::AfterTerminator)?;
    if fits {
        regs.r[1] = buffer.wrapping_add(len32(before));
        regs.r[6] = if selection.special.is_some() {
            buffer.wrapping_add(1)
        } else {
            0
        };
    }
    Ok(())
}
