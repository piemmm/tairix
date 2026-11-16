//! `cargo xtask riscos --reference <dir>`: the RISC OS emulator's reference
//! harness (`plans/RISCOS-EMULATOR.md` §17).
//!
//! `<dir>` is a genuine RISC OS a developer holds, laid out as RPCEmu lays
//! its own out: `roms/` the ROM images, `hostfs/` the directory the machine
//! boots from. The command writes the harness there: a BASIC program, built
//! with `lib/openbasic`, that runs every corpus scenario's calls and records
//! what each answered, then `lib/obey`'s cases ([`obey`]); and a `!Boot` that
//! first has BASIC V type `lib/openbasic`'s programs ([`basic`]), then runs
//! it. Where `<dir>` holds RPCEmu it is run headless until the harness is
//! done: the harness ends by opening `Done`, which here is a named pipe the
//! command waits on. Otherwise the harness is left for the developer to run,
//! and a later run imports what it recorded. Each recorded observation
//! becomes its scenario's fixture, but for a scenario marked `ours`, and each
//! crate's answers go beside its cases, every file naming the RISC OS version
//! and each ROM image's SHA-256.
//!
//! No fixture may carry the ROM's bytes: a run of [`ROM_RUN`] bytes of a ROM
//! image in what was recorded refuses the whole recording, unless the
//! corpus's own files hold it or it is mostly `&00`, spaces and `&FF`.

use std::fmt::Write as _;
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use tairix_openbasic::program::{self, Line};
use tairix_openbasic::tokenise::{tokenise, Start};
use tairix_openbasic::Dialect;
use tairix_riscos_kernel::corpus::{
    check_observation, format_observations, parse_observations, parse_scenarios, Observation,
    Scenario, Step, Value, REGISTERS,
};

use crate::commands::rom_runs::{runs, windows, Runs, ROM_RUN};
use crate::Context;

mod basic;
mod obey;

/// Where each crate keeps its corpus, from the workspace's root.
const CORPUS: &str = "userland/apps/riscos/kernel/tests/corpus";
/// The emulator `<dir>` may hold, run headless.
const EMULATOR: &str = "rpcemu-recompiler";
/// The longest a recording may take.
const DEADLINE: Duration = Duration::from_secs(300);
/// What each fixtures file's header says after its provenance.
const OURS: &str = "A scenario marked `ours` holds this emulator's own observation.";
/// How the harness records the RISC OS version it ran on: the version times
/// a hundred follows.
const VERSION: &str = "# version ";
/// The registers' BASIC variables, R0 first.
const VARIABLES: [&str; REGISTERS as usize] =
    ["A%", "B%", "C%", "D%", "E%", "F%", "G%", "H%", "I%", "J%"];

pub fn run(ctx: &Context, args: &[std::ffi::OsString]) -> Result<(), String> {
    let usage = "riscos: usage: cargo xtask riscos --reference <dir>";
    let args: Vec<&str> = args
        .iter()
        .map(|a| a.to_str().ok_or(usage))
        .collect::<Result<_, _>>()?;
    let ["--reference", dir] = args.as_slice() else {
        return Err(usage.into());
    };
    reference(ctx, Path::new(dir))
}

/// A corpus group: its scenarios, their source and its fixtures file.
struct Group {
    fixtures: PathBuf,
    scenarios: Vec<Scenario>,
    source: String,
}

fn groups(ctx: &Context) -> Result<Vec<Group>, String> {
    let dir = ctx.workspace_root.join(CORPUS);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("riscos: {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "scenarios"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("riscos: {}: {e}", path.display()))?;
            let scenarios = parse_scenarios(&text)
                .map_err(|e| format!("riscos: {}:{}: {}", path.display(), e.line, e.reason))?;
            Ok(Group {
                fixtures: path.with_extension("fixtures"),
                scenarios,
                source: text,
            })
        })
        .collect()
}

fn reference(ctx: &Context, dir: &Path) -> Result<(), String> {
    let groups = groups(ctx)?;
    let read = |path: &str| {
        std::fs::read_to_string(ctx.workspace_root.join(path))
            .map_err(|e| format!("riscos: {path}: {e}"))
    };
    let cases = read(obey::CASES)?;
    let programs_text = read(basic::PROGRAMS)?;
    let programs = basic::programs(&programs_text)?;
    let staged = [
        (obey::STAGED, cases.as_bytes().to_vec()),
        (basic::TYPED, basic::typed(&programs)),
    ];
    let hostfs = dir.join("hostfs");
    let harness = hostfs.join("Harness,ffb");
    let left: Vec<String> = left()
        .map(String::from)
        .chain(basic::left(&programs))
        .collect();
    let left = || left.iter().map(String::as_str);
    let (observed, answers) = if dir.join(EMULATOR).exists() {
        write_harness(&groups, &staged, left(), &hostfs, &harness)?;
        let ran = run_emulator(ctx, dir, &hostfs);
        remove_harness(&hostfs, &harness)?;
        ran?;
        (
            recorded(&hostfs, "Observed")?,
            recorded(&hostfs, obey::RECORDED)?,
        )
    } else if let Some(texts) = recording(&hostfs, &harness)? {
        remove_harness(&hostfs, &harness)?;
        texts
    } else {
        write_harness(&groups, &staged, left(), &hostfs, &harness)?;
        return outln!(
            "riscos: the harness is in {}: boot the machine from it, then run this again to import what it records",
            hostfs.display()
        );
    };
    let observations = parse_observations(&observed)
        .map_err(|e| format!("riscos: the recording:{}: {}", e.line, e.reason))?;
    if let Some(failed) = observations
        .iter()
        .flat_map(|o| &o.lines)
        .find(|line| line.starts_with('!'))
    {
        return Err(format!("riscos: the harness stopped: {failed}"));
    }
    if let Some(failed) = answers.lines().find(|line| line.starts_with("= !")) {
        return Err(format!("riscos: the lib/obey probe stopped: {failed}"));
    }
    let images = images(&dir.join("roms"))?;
    let provenance = provenance(&observed, &images)?;
    let listed = basic::answers(&provenance, &programs, &hostfs)?;
    let mut ours: Vec<Vec<u8>> = groups
        .iter()
        .map(|group| group.source.as_bytes().to_vec())
        .collect();
    for text in [&cases, &programs_text] {
        ours.push(text.as_bytes().to_vec());
        ours.extend(quoted(text));
    }
    let held = runs(&ours);
    let mut streams = vec![
        observed.as_bytes().to_vec(),
        answers.as_bytes().to_vec(),
        listed.as_bytes().to_vec(),
    ];
    streams.extend(register_streams(&observed));
    streams.extend(obey::written(&answers));
    streams.extend(quoted(&listed));
    refuse_rom_runs(&images, &streams, &held)?;
    let mut header: Vec<&str> = provenance.iter().map(String::as_str).collect();
    header.push(OURS);
    for group in &groups {
        write_fixtures(group, &observations, &header)?;
    }
    for (path, text) in [
        (obey::ANSWERS, obey::answers(&provenance, &answers)),
        (basic::ANSWERS, listed),
    ] {
        let path = ctx.workspace_root.join(path);
        std::fs::write(&path, text).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
    }
    remove_found(&hostfs, left())?;
    outln!(
        "riscos: {} observations, {} lib/obey answers and {} BASIC programs recorded",
        observations.len(),
        answers
            .lines()
            .filter(|line| line.starts_with("= "))
            .count(),
        programs.len()
    )
}

/// The text a harness run recorded under `name`.
fn recorded(hostfs: &Path, name: &str) -> Result<String, String> {
    let path = find(hostfs, name)
        .ok_or_else(|| format!("riscos: the harness ended without its {name}"))?;
    std::fs::read(&path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| format!("riscos: {}: {e}", path.display()))
}

/// What the harness recorded since it was last written, if it has finished:
/// the kernel's observations and `lib/obey`'s answers.
fn recording(hostfs: &Path, harness: &Path) -> Result<Option<(String, String)>, String> {
    let written = std::fs::metadata(harness).and_then(|m| m.modified()).ok();
    let done = find(hostfs, "Done");
    let observed = find(hostfs, "Observed");
    let answers = find(hostfs, obey::RECORDED);
    let (Some(done), Some(observed), Some(answers)) = (done, observed, answers) else {
        return Ok(None);
    };
    let fresh = |path: &Path| {
        let modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        written.is_none_or(|written| modified >= written)
    };
    if !fresh(&done) || !fresh(&observed) || !fresh(&answers) {
        return Ok(None);
    }
    let read = |path: &Path| {
        std::fs::read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|e| format!("riscos: {}: {e}", path.display()))
    };
    Ok(Some((read(&observed)?, read(&answers)?)))
}

/// The file HostFS stores as `name`, whatever type suffix it was given.
fn find(hostfs: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(hostfs)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n == name || n.split_once(',').is_some_and(|(stem, _)| stem == name)
            })
        })
}

/// The `!Boot` that types `lib/openbasic`'s programs into BASIC, then runs
/// the harness.
fn boot() -> String {
    format!(
        "| The TAIRiX reference harness.\n{}\nRun HostFS:$.Harness\n",
        basic::RUN
    )
}

/// The harness's `!Boot` in `hostfs`, if one is there; refused where the
/// machine has a `!Boot` of its own, which the harness never replaces.
fn harness_boot(hostfs: &Path) -> Result<Option<PathBuf>, String> {
    let Some(boot) = find(hostfs, "!Boot") else {
        return Ok(None);
    };
    match std::fs::read(&boot) {
        Ok(text) if text == self::boot().as_bytes() => Ok(Some(boot)),
        _ => Err(format!(
            "riscos: {} is the machine's own: move it aside to run the harness",
            boot.display()
        )),
    }
}

/// What a run of the harness leaves in HostFS: the kernel's part's files,
/// then `lib/obey`'s.
fn left() -> impl Iterator<Item = &'static str> {
    ["Done", "Observed"].into_iter().chain(obey::LEFT)
}

/// Writes the harness, the files it and BASIC read beside it, and the `!Boot`
/// that runs them, clearing what an earlier run left.
fn write_harness<'a>(
    groups: &[Group],
    staged: &[(&str, Vec<u8>)],
    left: impl IntoIterator<Item = &'a str>,
    hostfs: &Path,
    harness: &Path,
) -> Result<(), String> {
    std::fs::create_dir_all(hostfs).map_err(|e| format!("riscos: {}: {e}", hostfs.display()))?;
    harness_boot(hostfs)?;
    remove_found(hostfs, left)?;
    let program = harness_program(groups.iter().flat_map(|group| &group.scenarios))?;
    std::fs::write(harness, program).map_err(|e| format!("riscos: {}: {e}", harness.display()))?;
    for (name, bytes) in staged {
        let path = hostfs.join(format!("{name},fff"));
        std::fs::write(&path, bytes).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
    }
    let boot = hostfs.join("!Boot,feb");
    std::fs::write(&boot, self::boot()).map_err(|e| format!("riscos: {}: {e}", boot.display()))
}

/// Removes the harness and the `!Boot` that runs it, so the machine boots as
/// it did before.
fn remove_harness(hostfs: &Path, harness: &Path) -> Result<(), String> {
    if let Some(boot) = harness_boot(hostfs)? {
        std::fs::remove_file(&boot).map_err(|e| format!("riscos: {}: {e}", boot.display()))?;
    }
    match std::fs::remove_file(harness) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("riscos: {}: {e}", harness.display()))
        }
        _ => Ok(()),
    }
}

/// Removes each file HostFS stores as one of `names`.
fn remove_found<'a>(hostfs: &Path, names: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    for name in names {
        if let Some(path) = find(hostfs, name) {
            std::fs::remove_file(&path).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// The harness as BASIC V's tokenised program.
fn harness_program<'a>(
    scenarios: impl IntoIterator<Item = &'a Scenario>,
) -> Result<Vec<u8>, String> {
    let mut text: Vec<String> = vec![
        "O%=OPENOUT\"HostFS:$.Observed\"".into(),
        "ON ERROR ON ERROR OFF:BPUT#O%,\"! \"+STR$ERR+\" at \"+STR$ERL:CLOSE#O%:PROCdone:END"
            .into(),
        format!(
            "SYS\"OS_Module\",18,\"UtilityModule\" TO ,,,V%:BPUT#O%,\"{VERSION}\"+STR$(INT(VAL(FNv(V%+V%!&14))*100+0.5))"
        ),
    ];
    for scenario in scenarios {
        scenario_lines(scenario, &mut text)?;
    }
    text.push(obey::run());
    text.extend([
        "CLOSE#O%:PROCdone:END".into(),
        "DEF PROCdone:LOCAL D%:D%=OPENOUT\"HostFS:$.Done\":CLOSE#D%:ENDPROC".into(),
        "DEF FNh(N%)=\"&\"+RIGHT$(\"0000000\"+STR$~N%,8)".into(),
        "DEF FNr(R$,N%,F%)=\" \"+R$+\"=\"+FNh(N%)".into(),
        "DEF FNz(N%,F%):IF F% AND 1 THEN=\"\" ELSE=FNr(\"r0\",N%,F%)".into(),
        "DEF FNg(F%,B%,N$)=\" \"+N$+\"=\"+STR$(F% DIV B% AND 1)".into(),
        "DEF FNe(N%,F%):IF F% AND 1 THEN=\" error=\"+FNh(!N%) ELSE=\"\"".into(),
        // The version a module's help string gives after its name's tab.
        "DEF FNv(V%):LOCAL V$:WHILE ?V%>=32:V%+=1:ENDWHILE:IF ?V%<>9 THEN =\"\"".into(),
        "V%+=1:WHILE ?V%>=32 AND LEN V$<20:V$+=CHR$?V%:V%+=1:ENDWHILE:=V$".into(),
    ]);
    text.extend(obey::PROBE.lines().map(String::from));
    let mut tokenised = Vec::with_capacity(text.len());
    for line in &text {
        let mut bytes = Vec::new();
        tokenise(
            Dialect::BasicV,
            line.as_bytes(),
            Start::Statement,
            &mut bytes,
        )
        .map_err(|e| format!("riscos: the harness line {line:?}: {e:?}"))?;
        tokenised.push(bytes);
    }
    let lines = tokenised
        .iter()
        .zip((10..).step_by(10))
        .map(|(text, number)| Line { number, text });
    program::image(lines).map_err(|e| format!("riscos: the harness: {e:?}"))
}

/// A BASIC expression for `value`, kept values being `K<n>%`.
fn expression(value: &Value, kept: &[&str]) -> Result<String, String> {
    Ok(match value {
        Value::Number(number) => format!("&{number:X}"),
        Value::Text(text) => format!("\"{text}\""),
        Value::Kept { name, offset } => {
            let index = kept
                .iter()
                .position(|k| k == name)
                .ok_or("riscos: a kept value unknown")?;
            match *offset {
                0 => format!("K{index}%"),
                offset if offset > 0 => format!("K{index}%+&{offset:X}"),
                offset => format!("K{index}%-&{:X}", offset.unsigned_abs()),
            }
        }
    })
}

fn scenario_lines(scenario: &Scenario, text: &mut Vec<String>) -> Result<(), String> {
    text.push(format!("BPUT#O%,\"scenario {}\"", scenario.name));
    let mut kept: Vec<&str> = Vec::new();
    let mut call = 0;
    for step in &scenario.steps {
        match step {
            Step::Call { swi, registers } => {
                call += 1;
                let mut arguments = vec![String::from("0"); REGISTERS as usize];
                for (register, value) in registers {
                    arguments[*register as usize] = expression(value, &kept)?;
                }
                while arguments.len() > 1 && arguments.last().is_some_and(|a| a == "0") {
                    arguments.pop();
                }
                text.push(format!(
                    "SYS\"{swi}\",{} TO {};Z%",
                    arguments.join(","),
                    VARIABLES.join(",")
                ));
            }
            Step::Keep { register, name } => {
                if !kept.contains(&name.as_str()) {
                    kept.push(name);
                }
                let index = kept.iter().position(|k| k == name).unwrap_or(0);
                text.push(format!("K{index}%={}", VARIABLES[*register as usize]));
            }
            Step::Show { registers, flags } => {
                let mut line = format!("BPUT#O%,\"{call}\"");
                for &register in registers {
                    let variable = VARIABLES[register as usize];
                    if register == 0 {
                        let _ = write!(line, "+FNz({variable},Z%)");
                    } else {
                        let _ = write!(line, "+FNr(\"r{register}\",{variable},Z%)");
                    }
                }
                for flag in flags {
                    let _ = write!(line, "+FNg(Z%,{},\"{}\")", flag.bit(), flag.name());
                }
                line.push_str("+FNe(A%,Z%)");
                text.push(line);
            }
        }
    }
    Ok(())
}

/// What ended the wait for the harness.
enum Ended {
    /// The harness opened `Done` and closed it: whether it read cleanly.
    Done(bool),
    /// The emulator exited first.
    Exited,
}

/// Runs `<dir>`'s RPCEmu headless until the harness opens `Done`, a named
/// pipe here, the emulator exits, or the deadline passes.
fn run_emulator(ctx: &Context, dir: &Path, hostfs: &Path) -> Result<(), String> {
    let done = hostfs.join("Done,ffd");
    let mut mkfifo = Command::new("mkfifo");
    mkfifo.arg(&done);
    ctx.run("riscos: mkfifo", mkfifo)?;
    let (said, heard) = mpsc::channel();
    let pipe = done.clone();
    let read = said.clone();
    // Opening the pipe blocks until the harness opens it; its close ends the
    // read.
    let reader = std::thread::spawn(move || {
        let ended =
            std::fs::File::open(&pipe).and_then(|mut file| file.read_to_end(&mut Vec::new()));
        let _ = read.send(Ended::Done(ended.is_ok()));
    });
    let mut emulator = Command::new(dir.join(EMULATOR));
    emulator
        .current_dir(dir)
        .env("QT_QPA_PLATFORM", "offscreen")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let outcome = match ctx.children.spawn(&mut emulator) {
        Ok(mut child) => {
            let pid = child.id();
            std::thread::spawn(move || {
                let _ = child.wait();
                let _ = said.send(Ended::Exited);
            });
            let outcome = heard.recv_timeout(DEADLINE);
            // The emulator runs until it is stopped, unless it already has.
            if !matches!(outcome, Ok(Ended::Exited)) {
                crate::children::kill(pid);
            }
            ctx.children.release(pid);
            Ok(outcome)
        }
        Err(e) => Err(format!("riscos: {}: {e}", dir.join(EMULATOR).display())),
    };
    release_reader(&done);
    let _ = reader.join();
    let _ = std::fs::remove_file(&done);
    match outcome? {
        Ok(Ended::Done(true)) => Ok(()),
        Ok(Ended::Done(false)) => Err(format!("riscos: {} could not be read", done.display())),
        Ok(Ended::Exited) => Err("riscos: the emulator ended before the harness did".into()),
        Err(_) => Err(format!(
            "riscos: the harness did not finish within {} s",
            DEADLINE.as_secs()
        )),
    }
}

/// Opens `done`'s writing end without waiting, which frees a reader still
/// waiting for the harness and, with no reader left, opens nothing.
fn release_reader(done: &Path) {
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(done);
}

/// A ROM image in `<dir>`'s `roms/`.
struct Image {
    name: String,
    bytes: Vec<u8>,
}

/// Every ROM image in `roms`, in name order; refused where there is none,
/// since nothing recorded could then be checked against one.
fn images(roms: &Path) -> Result<Vec<Image>, String> {
    let failed = |e: std::io::Error| format!("riscos: {}: {e}", roms.display());
    let mut images = Vec::new();
    for entry in std::fs::read_dir(roms).map_err(failed)? {
        let path = entry.map_err(failed)?.path();
        if path.is_file() {
            let bytes =
                std::fs::read(&path).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            images.push(Image { name, bytes });
        }
    }
    images.sort_by(|a, b| a.name.cmp(&b.name));
    if images.is_empty() {
        return Err(format!(
            "riscos: no ROM image in {} to check the recording against",
            roms.display()
        ));
    }
    Ok(images)
}

/// Refuses a recording that carries, in any of `streams`, a run of a ROM
/// image's bytes that is not one of `held`, the corpus's own; but for a run
/// mostly of `&00`, spaces and `&FF`, which is small values or a listing's
/// layout that any ROM also holds rather than anything of its content.
fn refuse_rom_runs(images: &[Image], streams: &[Vec<u8>], held: &Runs) -> Result<(), String> {
    let wanted: Runs = streams
        .iter()
        .flat_map(|stream| windows(stream))
        .filter(|run| carries_content(run) && !held.contains(run))
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    for image in images {
        if let Some(run) = windows(&image.bytes).find(|run| wanted.contains(run)) {
            return Err(format!(
                "riscos: refused: the recording carries {} from the ROM image {}",
                tairix_util::hex::encode(&run),
                image.name
            ));
        }
    }
    Ok(())
}

/// Whether at least half of `run`'s bytes are none of `&00`, a space and
/// `&FF`.
fn carries_content(run: &[u8; ROM_RUN]) -> bool {
    run.iter()
        .filter(|&&b| !matches!(b, 0 | b' ' | 0xFF))
        .count()
        * 2
        >= ROM_RUN
}

/// The bytes of the words each recorded step shows in consecutive registers,
/// a stream for each run of them, each word as memory holds it, low byte
/// first: the order a block of memory read into them would have.
fn register_streams(observed: &str) -> Vec<Vec<u8>> {
    let mut streams = Vec::new();
    for line in observed.lines() {
        let mut stream = Vec::new();
        let mut last: Option<u8> = None;
        for (register, word) in line.split(' ').filter_map(register_word) {
            if last.is_some_and(|last| last.checked_add(1) != Some(register)) {
                streams.push(std::mem::take(&mut stream));
            }
            stream.extend(word.to_le_bytes());
            last = Some(register);
        }
        streams.push(stream);
    }
    streams.retain(|stream| !stream.is_empty());
    streams
}

/// A token `rN=&XXXXXXXX`: its register and word.
fn register_word(token: &str) -> Option<(u8, u32)> {
    let (name, digits) = token.split_once("=&")?;
    let register = name.strip_prefix('r')?.parse().ok()?;
    let word = u32::from_str_radix(digits, 16)
        .ok()
        .filter(|_| digits.len() == 8)?;
    Some((register, word))
}

/// What a recording was taken from, as each file it writes opens: the RISC OS
/// version the harness found and each ROM image's SHA-256.
fn provenance(observed: &str, images: &[Image]) -> Result<Vec<String>, String> {
    let version: u32 = observed
        .lines()
        .find_map(|line| line.strip_prefix(VERSION))
        .and_then(|n| n.trim().parse().ok())
        .filter(|&n| n > 0)
        .ok_or("riscos: the harness recorded no RISC OS version")?;
    let mut lines = vec![format!(
        "Recorded by `cargo xtask riscos --reference` from genuine RISC OS {}.{:02},",
        version / 100,
        version % 100
    )];
    lines.extend(images.iter().map(|image| {
        format!(
            "the ROM image `{}` of SHA-256 {}.",
            image.name,
            tairix_util::hex::encode(&tairix_crypto::sha256(&image.bytes))
        )
    }));
    Ok(lines)
}

/// Rewrites a group's fixtures from what was recorded, keeping those of its
/// scenarios marked `ours`.
fn write_fixtures(
    group: &Group,
    observations: &[Observation],
    header: &[&str],
) -> Result<(), String> {
    let old = match std::fs::read_to_string(&group.fixtures) {
        Ok(text) => parse_observations(&text).map_err(|e| {
            format!(
                "riscos: {}:{}: {}",
                group.fixtures.display(),
                e.line,
                e.reason
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("riscos: {}: {e}", group.fixtures.display())),
    };
    let mut fixtures = Vec::with_capacity(group.scenarios.len());
    for scenario in &group.scenarios {
        let source = if scenario.ours { &old } else { observations };
        let fixture = source
            .iter()
            .find(|o| o.name == scenario.name)
            .cloned()
            .ok_or_else(|| format!("riscos: nothing recorded for {}", scenario.name))?;
        check_observation(scenario, &fixture).map_err(|e| {
            format!(
                "riscos: {} recorded wrongly: its line {}: {}",
                scenario.name, e.line, e.reason
            )
        })?;
        fixtures.push(fixture);
    }
    std::fs::write(&group.fixtures, format_observations(header, &fixtures))
        .map_err(|e| format!("riscos: {}: {e}", group.fixtures.display()))
}

/// `bytes` quoted as the harness writes a field: printable ASCII but for
/// `"` and `\\`, every other byte `\\xHH`.
fn quote(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    for &b in bytes {
        if (0x20..=0x7E).contains(&b) && b != b'"' && b != b'\\' {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "\\x{b:02X}");
        }
    }
    out.push('"');
    out
}

/// The bytes of every quoted string in `text`, a case's fields or an
/// answer's buffers, `\xHH` read as its byte; a string left open ends with
/// its line.
fn quoted(text: &str) -> Vec<Vec<u8>> {
    let mut strings = Vec::new();
    for line in text.lines() {
        let mut bytes = line.bytes();
        while bytes.any(|b| b == b'"') {
            let mut string = Vec::new();
            while let Some(b) = bytes.next() {
                match b {
                    b'"' => break,
                    b'\\' => {
                        let digits: Vec<u8> = bytes.by_ref().skip(1).take(2).collect();
                        let byte = std::str::from_utf8(&digits)
                            .ok()
                            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                        string.extend(byte);
                    }
                    other => string.push(other),
                }
            }
            strings.push(string);
        }
    }
    strings
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;
    use crate::children::Children;

    /// A scratch `<dir>` whose emulator is the shell script `script`.
    fn emulated(script: &str) -> (tairix_fuzzseed::scratch::Scratch, Context) {
        let scratch =
            tairix_fuzzseed::scratch::Scratch::under(&std::env::temp_dir(), "riscos-emulated");
        std::fs::create_dir_all(scratch.path().join("hostfs")).expect("a hostfs");
        let emulator = scratch.path().join(EMULATOR);
        std::fs::write(&emulator, format!("#!/bin/sh\n{script}\n")).expect("an emulator");
        std::fs::set_permissions(&emulator, std::fs::Permissions::from_mode(0o700))
            .expect("an executable emulator");
        let ctx = Context {
            workspace_root: scratch.path().to_path_buf(),
            cargo: std::ffi::OsString::from("true"),
            children: Arc::new(Children::new(None)),
        };
        (scratch, ctx)
    }

    #[test]
    fn the_harness_never_replaces_the_machine_s_own_boot_and_leaves_none_of_its_own() {
        let scratch =
            tairix_fuzzseed::scratch::Scratch::under(&std::env::temp_dir(), "riscos-boot");
        let hostfs = scratch.path().join("hostfs");
        let harness = hostfs.join("Harness,ffb");
        std::fs::create_dir_all(&hostfs).unwrap();
        std::fs::write(hostfs.join("!Boot,feb"), "Run Mine\n").unwrap();
        let cases = "cli \"Echo\"\n";
        let staged = [(obey::STAGED, cases.as_bytes().to_vec())];
        let left = || ["Done", obey::RECORDED];
        let refused =
            write_harness(&[], &staged, left(), &hostfs, &harness).expect_err("the machine's own");
        assert!(refused.contains("move it aside"), "{refused}");
        assert!(!harness.exists());
        assert!(remove_harness(&hostfs, &harness).is_err(), "never removed");
        assert_eq!(
            std::fs::read_to_string(hostfs.join("!Boot,feb")).unwrap(),
            "Run Mine\n"
        );
        std::fs::remove_file(hostfs.join("!Boot,feb")).unwrap();
        std::fs::write(hostfs.join(format!("{},fff", obey::RECORDED)), "= stale\n").unwrap();
        write_harness(&[], &staged, left(), &hostfs, &harness).unwrap();
        write_harness(&[], &staged, left(), &hostfs, &harness).unwrap();
        assert!(find(&hostfs, "!Boot").is_some() && harness.exists());
        assert!(
            find(&hostfs, obey::RECORDED).is_none(),
            "an old run's answers"
        );
        assert_eq!(
            std::fs::read_to_string(hostfs.join(format!("{},fff", obey::STAGED))).unwrap(),
            cases
        );
        remove_harness(&hostfs, &harness).unwrap();
        assert!(find(&hostfs, "!Boot").is_none() && !harness.exists());
    }

    #[test]
    fn the_emulator_runs_until_the_harness_opens_done() {
        let (scratch, ctx) = emulated(": > hostfs/Done,ffd\nexec sleep 60");
        let started = Instant::now();
        let dir = scratch.path();
        assert_eq!(run_emulator(&ctx, dir, &dir.join("hostfs")), Ok(()));
        assert!(started.elapsed() < DEADLINE / 4, "stopped once done");
        assert!(
            ctx.children.live_children().is_empty(),
            "the emulator released"
        );
        assert!(
            find(&dir.join("hostfs"), "Done").is_none(),
            "the pipe removed"
        );
    }

    #[test]
    fn an_emulator_ending_before_the_harness_ends_the_wait() {
        let (scratch, ctx) = emulated("exit 0");
        let started = Instant::now();
        let dir = scratch.path();
        let refused = run_emulator(&ctx, dir, &dir.join("hostfs")).expect_err("no recording");
        assert!(refused.contains("ended before the harness"), "{refused}");
        assert!(started.elapsed() < DEADLINE / 4, "not the whole deadline");
        assert!(
            find(&dir.join("hostfs"), "Done").is_none(),
            "the pipe removed"
        );
    }

    #[test]
    fn the_harness_tokenises_and_every_scenario_step_becomes_a_line() {
        let scenarios = parse_scenarios(
            "scenario one\ncall XOS_DynamicArea r0=0 r1=-1 r8=\"Corpus\"\nkeep r1 area\ncall XOS_DynamicArea r0=2 r1=$area+&10\nshow r0 r2 v\n",
        )
        .unwrap();
        let mut text = Vec::new();
        scenario_lines(&scenarios[0], &mut text).unwrap();
        assert_eq!(
            text,
            [
                "BPUT#O%,\"scenario one\"",
                "SYS\"XOS_DynamicArea\",&0,&FFFFFFFF,0,0,0,0,0,0,\"Corpus\" TO A%,B%,C%,D%,E%,F%,G%,H%,I%,J%;Z%",
                "K0%=B%",
                "SYS\"XOS_DynamicArea\",&2,K0%+&10 TO A%,B%,C%,D%,E%,F%,G%,H%,I%,J%;Z%",
                "BPUT#O%,\"2\"+FNz(A%,Z%)+FNr(\"r2\",C%,Z%)+FNg(Z%,1,\"v\")+FNe(A%,Z%)",
            ]
        );
        let image = harness_program(&scenarios).unwrap();
        assert!(
            program::lines(&image).all(|line| line.is_ok()),
            "a well-formed program"
        );
    }

    /// The streams of `recorded`: its text and its registers' bytes.
    fn streams(recorded: &str) -> Vec<Vec<u8>> {
        let mut streams = vec![recorded.as_bytes().to_vec()];
        streams.extend(register_streams(recorded));
        streams
    }

    #[test]
    fn a_quoted_string_s_escapes_are_its_bytes() {
        assert_eq!(
            quoted("gstrans \"a\\x0Db\" \"0\"\n= out \"\\x22\\x5C\" \"open"),
            [
                b"a\rb".to_vec(),
                b"0".to_vec(),
                b"\"\\".to_vec(),
                b"open".to_vec()
            ]
        );
    }

    #[test]
    fn a_quotation_is_read_back_as_its_bytes() {
        let bytes = [b'a', b'"', b'\\', 0, 0x7F, 0xE9];
        assert_eq!(quote(&bytes), "\"a\\x22\\x5C\\x00\\x7F\\xE9\"");
        assert_eq!(quoted(&quote(&bytes)), [bytes.to_vec()]);
    }

    #[test]
    fn a_recording_carrying_a_roms_bytes_is_refused() {
        let mut bytes = b"....Unknown dynamic area....".to_vec();
        bytes.extend([0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC, 0xFE, 1, 2, 3, 4]);
        bytes.extend([0, 0, 1, 0, 0, 0, 0, 0, 0, 0x20, 0, 0]);
        bytes.extend(b"           F");
        let images = [Image {
            name: "riscos".into(),
            bytes,
        }];
        let none = Runs::new();
        let refused = |recorded: &str| refuse_rom_runs(&images, &streams(recorded), &none);
        assert!(refused("scenario a\n1 error=&00000105\n").is_ok());
        assert!(refused("scenario a\n1 Unknown dynamic area\n").is_err());
        let held = runs(&["scenario Unknown-dynamic-area"]);
        assert!(
            refuse_rom_runs(&images, &streams("scenario Unknown-dynamic-area\n"), &held).is_ok(),
            "a run the corpus's own text holds"
        );
        assert!(
            refused("scenario a\n1 r1=&76543210 r2=&FEDCBA98 r3=&04030201\n").is_err(),
            "a ROM's words read into consecutive registers"
        );
        assert!(
            refused("scenario a\n1 r1=&76543210 r2=&FEDCBA98\n").is_ok(),
            "fewer of its bytes than a run"
        );
        assert!(
            refused("scenario a\n1 r1=&76543210 r2=&FEDCBA98 r4=&04030201\n").is_ok(),
            "registers apart"
        );
        assert!(
            refused("scenario a\n1 r1=&76543210 r2=&FEDCBA98\n2 r1=&04030201\n").is_ok(),
            "steps apart"
        );
        assert!(
            refused("scenario a\n1 r4=&00010000 r5=&00000000 r6=&00002000\n").is_ok(),
            "small values, which the ROM holds only as fill"
        );
        assert!(
            refused("scenario a\n1           F\n").is_ok(),
            "a listing's indent"
        );
    }

    #[test]
    fn a_step_s_consecutive_registers_are_one_stream_low_byte_first() {
        assert_eq!(
            register_streams(
                "1 r0=&04030201 r1=&08070605 v=1 r3=&0C0B0A09 x=&GGGGGGGG\n2 r2=&123\n"
            ),
            [vec![1, 2, 3, 4, 5, 6, 7, 8], vec![9, 10, 11, 12]]
        );
        assert_eq!(register_word("error=&00000105"), None);
        assert_eq!(register_word("r12=&FFFFFFFF"), Some((12, u32::MAX)));
    }

    #[test]
    fn a_recording_names_its_version_and_each_image_s_digest() {
        let images = [Image {
            name: "riscos".into(),
            bytes: b"abc".to_vec(),
        }];
        assert_eq!(
            provenance("# version 530\nscenario a\n", &images).unwrap(),
            [
                "Recorded by `cargo xtask riscos --reference` from genuine RISC OS 5.30,",
                "the ROM image `riscos` of SHA-256 ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad."
            ]
        );
        assert!(provenance("scenario a\n", &images).is_err(), "no version");
        assert!(
            provenance("# version 0\n", &images).is_err(),
            "no version found"
        );
    }

    #[test]
    fn a_rom_directory_with_no_image_is_refused() {
        let scratch =
            tairix_fuzzseed::scratch::Scratch::under(&std::env::temp_dir(), "riscos-reference");
        let roms = scratch.path().join("roms");
        std::fs::create_dir_all(&roms).unwrap();
        assert!(images(&roms).is_err(), "nothing to check against");
        std::fs::write(roms.join("b"), b"2").unwrap();
        std::fs::write(roms.join("a"), b"1").unwrap();
        let names: Vec<String> = images(&roms).unwrap().into_iter().map(|i| i.name).collect();
        assert_eq!(names, ["a", "b"]);
    }
}
