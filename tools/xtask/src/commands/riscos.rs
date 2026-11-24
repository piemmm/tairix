//! `cargo xtask riscos --reference <dir>`: TAIRiX Bridge's reference
//! harness (`plans/RISCOS-EMULATOR.md` §17). `--write-sprites` draws the
//! Wimp's file-type sprites its ResourceFS holds ([`sprites`]), and
//! `--write-encodings` writes the ROM fonts' encoding files.
//!
//! `<dir>` is a genuine RISC OS a developer holds, laid out as RPCEmu lays
//! its own out: `roms/` the ROM images, `hostfs/` the directory the machine
//! boots from. The command writes the harness there: a BASIC program, built
//! with `lib/openbasic`, that runs every corpus scenario's calls and records
//! what each answered, then `lib/obey`'s cases ([`obey`]); and a `!Boot` that
//! first has BASIC V type `lib/openbasic`'s programs ([`basic`]), then has
//! BASIC V and BASIC VI answer `tairix-riscos-basic`'s cases ([`numbers`]),
//! run its programs ([`language`]), assemble its assembler's lines
//! ([`assembler`]) and type its sessions ([`session`]), then runs it. Where
//! `<dir>` holds RPCEmu it is run headless until the harness is done: the
//! harness ends by writing a byte to `Done`, which here is a named pipe the
//! command waits on. Otherwise the harness is left for the developer to run,
//! and a later run imports what it recorded. Each recorded observation
//! becomes its scenario's fixture; a scenario marked `ours`, whose fixture
//! is the emulator's own, is not run, so nothing it does to genuine RISC OS
//! can stop the run. Each crate's answers go beside its cases, every file
//! naming the RISC OS version and each ROM image's SHA-256.
//!
//! No fixture may carry the ROM's bytes: a run of [`ROM_RUN`] bytes of a ROM
//! image in what was recorded refuses the whole recording, unless the
//! corpus's own files hold it or it is mostly `&00`, spaces and `&FF`.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime};

use tairix_openbasic::program::{self, Line};
use tairix_openbasic::tokenise::{tokenise, Start};
use tairix_openbasic::Dialect;
use tairix_riscos_basic::corpus::quoted as quote;
use tairix_riscos_kernel::corpus::{
    check_observation, format_observations, parse_observations, parse_scenarios, At, Item, Kind,
    Observation, Scenario, Step, Value, REGISTERS, UNWRITTEN,
};

use crate::commands::rom_runs::{runs, windows, Runs, ROM_RUN};
use crate::Context;

mod assembler;
mod basic;
mod language;
mod numbers;
mod obey;
mod screens;
mod session;
mod sprites;

pub use sprites::check as check_sprites;

/// Where each crate keeps its corpus, from the workspace's root.
const CORPORA: [&str; 2] = [
    "userland/apps/tairix-bridge/riscos/kernel/tests/corpus",
    "userland/apps/tairix-bridge/riscos/filing/tests/corpus",
];
/// The emulator `<dir>` may hold, run headless.
const EMULATOR: &str = "rpcemu-recompiler";
/// The longest a recording may take: every BASIC run of every part, each
/// dialect's, then the scenarios, with room over.
const DEADLINE: Duration = Duration::from_mins(20);
/// What each fixtures file's header says after its provenance.
const OURS: &str = "A scenario marked `ours` holds this emulator's own observation.";
const REFERENCE: &str =
    "A scenario marked `reference` reads the machine itself and is never replayed.";
/// How the harness records the RISC OS version it ran on: the version times
/// a hundred follows.
const VERSION: &str = "# version ";
/// The registers' BASIC variables, R0 first.
const VARIABLES: [&str; REGISTERS as usize] =
    ["A%", "B%", "C%", "D%", "E%", "F%", "G%", "H%", "I%", "J%"];

pub fn run(ctx: &Context, args: &[std::ffi::OsString]) -> Result<(), String> {
    let usage =
        "riscos: usage: cargo xtask riscos --reference <dir> | --write-sprites | --write-encodings";
    let args: Vec<&str> = args
        .iter()
        .map(|a| a.to_str().ok_or(usage))
        .collect::<Result<_, _>>()?;
    match args.as_slice() {
        ["--reference", dir] => reference(ctx, Path::new(dir)),
        ["--write-sprites"] => sprites::write(ctx),
        ["--write-encodings"] => write_encodings(ctx),
        _ => Err(usage.into()),
    }
}

/// Where the ROM fonts' encoding files are committed, from the workspace's
/// root.
const ENCODINGS: &str = "userland/apps/tairix-bridge/riscos/font/resources/ROMFonts/Encodings";

/// `--write-encodings`: each encoding file made afresh and written over
/// its committed copy, its leaf's `/` left off the file's name.
fn write_encodings(ctx: &Context) -> Result<(), String> {
    let directory = ctx.workspace_root.join(ENCODINGS);
    for (leaf, bytes) in tairix_riscos_font::encoding_files() {
        let path = directory.join(leaf.trim_start_matches('/'));
        std::fs::write(&path, bytes).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
    }
    Ok(())
}

/// A corpus group: its scenarios, their source and its fixtures file.
struct Group {
    fixtures: PathBuf,
    scenarios: Vec<Scenario>,
    source: String,
}

fn groups(ctx: &Context) -> Result<Vec<Group>, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for corpus in CORPORA {
        let dir = ctx.workspace_root.join(corpus);
        let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| format!("riscos: {}: {e}", dir.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "scenarios"))
            .collect();
        found.sort();
        paths.extend(found);
    }
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

/// The corpora a recording runs, as read and as parsed.
struct Corpora {
    cases: String,
    programs_text: String,
    programs: Vec<basic::Program>,
    language_text: String,
    language: Vec<tairix_riscos_basic::corpus::language::Program>,
    assembler_text: String,
    assembler: Vec<tairix_riscos_basic::corpus::assembler::Line>,
    session_text: String,
    sessions: Vec<tairix_riscos_basic::corpus::session::Session>,
    screens: screens::Screens,
    numbers: Vec<(&'static str, Vec<u8>)>,
}

impl Corpora {
    fn read(ctx: &Context) -> Result<Self, String> {
        let read = |path: &str| {
            std::fs::read_to_string(ctx.workspace_root.join(path))
                .map_err(|e| format!("riscos: {path}: {e}"))
        };
        let programs_text = read(basic::PROGRAMS)?;
        let language_text = read(language::PROGRAMS)?;
        let assembler_text = read(assembler::LINES)?;
        let session_text = read(session::SESSIONS)?;
        Ok(Self {
            cases: read(obey::CASES)?,
            programs: basic::programs(&programs_text)?,
            language: language::programs(&language_text)?,
            assembler: assembler::lines(&assembler_text)?,
            sessions: session::sessions(&session_text)?,
            screens: screens::Screens::read(&ctx.workspace_root)?,
            numbers: numbers::staged(),
            programs_text,
            language_text,
            assembler_text,
            session_text,
        })
    }

    /// What a run of the harness leaves in HostFS.
    fn left(&self) -> Vec<String> {
        left()
            .map(String::from)
            .chain(basic::left(&self.programs))
            .chain(numbers::LEFT.map(String::from))
            .chain(language::left(&self.language))
            .chain(assembler::left(&self.assembler))
            .chain(session::left(&self.sessions))
            .chain(screens::left(&self.screens))
            .collect()
    }
}

fn reference(ctx: &Context, dir: &Path) -> Result<(), String> {
    let groups = groups(ctx)?;
    let corpora = Corpora::read(ctx)?;
    let mut staged = vec![
        (obey::STAGED, corpora.cases.as_bytes().to_vec()),
        (basic::TYPED, basic::typed(&corpora.programs)),
    ];
    staged.extend(corpora.numbers.iter().cloned());
    let language_staged = language::staged(&corpora.language);
    let assembler_staged = assembler::staged(&corpora.assembler);
    let session_staged = session::staged(&corpora.sessions);
    let screens_staged = screens::staged(&corpora.screens);
    staged.extend(
        language_staged
            .iter()
            .chain(&assembler_staged)
            .chain(&session_staged)
            .chain(&screens_staged)
            .map(|(name, bytes)| (name.as_str(), bytes.clone())),
    );
    let library = screens::library();
    let mut commands = language::run(&corpora.language);
    commands.extend(assembler::run(&corpora.assembler));
    commands.extend(session::run(&corpora.sessions));
    commands.extend(screens::run(&corpora.screens));
    let ours = boot(&commands);
    let hostfs = dir.join("hostfs");
    let harness = hostfs.join("Harness,ffb");
    let left = corpora.left();
    let left = || left.iter().map(String::as_str);
    let (observed, answers) = if dir.join(EMULATOR).exists() {
        write_harness(&groups, &staged, &library, left(), &hostfs, &harness, &ours)?;
        let ran = run_emulator(ctx, dir, &hostfs);
        remove_harness(&hostfs, &harness, &ours)?;
        ran?;
        (
            recorded(&hostfs, "Observed")?,
            recorded(&hostfs, obey::RECORDED)?,
        )
    } else if let Some(texts) = recording(&hostfs, &harness)? {
        remove_harness(&hostfs, &harness, &ours)?;
        texts
    } else {
        write_harness(&groups, &staged, &library, left(), &hostfs, &harness, &ours)?;
        return outln!(
            "riscos: the harness is in {}: boot the machine from it, then run this again to import what it records",
            hostfs.display()
        );
    };
    import(ctx, dir, &groups, &corpora, &observed, &answers)
}

/// What a recording holds, `observed` and `lib/obey`'s `answers` among it,
/// checked for runs of ROM bytes and written out as the fixtures and each
/// corpus's answers.
fn import(
    ctx: &Context,
    dir: &Path,
    groups: &[Group],
    corpora: &Corpora,
    observed: &str,
    answers: &str,
) -> Result<(), String> {
    let hostfs = dir.join("hostfs");
    let observations = parse_observations(observed)
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
    let provenance = provenance(observed, &images)?;
    let listed = basic::answers(&provenance, &corpora.programs, &hostfs)?;
    let counted = numbers::answers(&provenance, &hostfs)?;
    let printed = language::answers(&provenance, &corpora.language, &hostfs)?;
    let assembled = assembler::answers(&provenance, &corpora.assembler, &hostfs)?;
    let typed = session::answers(&provenance, &corpora.sessions, &hostfs)?;
    let (drawn, readings) = screens::answers(&provenance, &corpora.screens, &hostfs)?;
    let texts: Vec<&str> = [
        corpora.cases.as_str(),
        &corpora.programs_text,
        &corpora.language_text,
        &corpora.assembler_text,
        &corpora.session_text,
    ]
    .into_iter()
    .chain(corpora.screens.texts())
    .collect();
    let held = held_runs(groups, &texts, &corpora.numbers);
    let mut streams = vec![
        observed.as_bytes().to_vec(),
        answers.as_bytes().to_vec(),
        listed.as_bytes().to_vec(),
        counted.as_bytes().to_vec(),
        printed.as_bytes().to_vec(),
        assembled.as_bytes().to_vec(),
        typed.as_bytes().to_vec(),
        readings.into_bytes(),
    ];
    streams.extend(register_streams(observed));
    streams.extend(memory_streams(observed));
    streams.extend(obey::written(answers));
    streams.extend(quoted(&listed));
    streams.extend(quoted(&counted));
    streams.extend(quoted(&printed));
    streams.extend(quoted(&typed));
    streams.extend(assembler::laid(&assembled));
    refuse_rom_runs(&images, &streams, &held)?;
    let mut header: Vec<&str> = provenance.iter().map(String::as_str).collect();
    header.extend([OURS, REFERENCE]);
    for group in groups {
        write_fixtures(group, &observations, &header)?;
    }
    for (path, text) in [
        (obey::ANSWERS, obey::answers(&provenance, answers)),
        (basic::ANSWERS, listed),
        (numbers::ANSWERS, counted),
        (language::ANSWERS, printed),
        (assembler::ANSWERS, assembled),
        (session::ANSWERS, typed),
    ]
    .into_iter()
    .chain(drawn)
    {
        let path = ctx.workspace_root.join(path);
        std::fs::write(&path, text).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
    }
    remove_found(&hostfs, corpora.left().iter().map(String::as_str))?;
    outln!(
        "riscos: {} observations, {} lib/obey answers, {} typed BASIC programs, {} run ones, {} assembler lines, {} sessions and {} screens recorded",
        observations.len(),
        answers
            .lines()
            .filter(|line| line.starts_with("= "))
            .count(),
        corpora.programs.len(),
        corpora.language.len(),
        corpora.assembler.len(),
        corpora.sessions.len(),
        corpora.screens.len()
    )
}

/// The runs the corpus's own files hold: every group's scenarios, `texts`
/// and the numbers' programs, each with the strings they quote.
fn held_runs(groups: &[Group], texts: &[&str], programs: &[(&str, Vec<u8>)]) -> Runs {
    let mut ours: Vec<Vec<u8>> = groups
        .iter()
        .map(|group| group.source.as_bytes().to_vec())
        .collect();
    for text in texts {
        ours.push(text.as_bytes().to_vec());
        ours.extend(quoted(text));
    }
    for (_, program) in programs {
        ours.push(program.clone());
        ours.extend(quoted(&String::from_utf8_lossy(program)));
    }
    runs(&ours)
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

/// The `!Boot` that types `lib/openbasic`'s programs into BASIC, has BASIC
/// answer the numbers' cases, runs `commands`, then runs the harness.
fn boot(commands: &[String]) -> String {
    let mut boot = format!(
        "| The TAIRiX reference harness.\n{}\n{}\n",
        basic::RUN,
        numbers::RUN.join("\n")
    );
    for command in commands {
        boot.push_str(command);
        boot.push('\n');
    }
    boot.push_str("Run HostFS:$.Harness\n");
    boot
}

/// The harness's `!Boot` in `hostfs`, if one is there; refused where the
/// machine has a `!Boot` of its own, which the harness never replaces.
fn harness_boot(hostfs: &Path, ours: &str) -> Result<Option<PathBuf>, String> {
    let Some(boot) = find(hostfs, "!Boot") else {
        return Ok(None);
    };
    match std::fs::read(&boot) {
        Ok(text) if text == ours.as_bytes() => Ok(Some(boot)),
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

/// Writes the harness, the files it and BASIC read beside it, the typed
/// `library` files each at its own path, and `ours`, the `!Boot` that runs
/// them, clearing what an earlier run left.
fn write_harness<'a>(
    groups: &[Group],
    staged: &[(&str, Vec<u8>)],
    library: &[(String, Vec<u8>)],
    left: impl IntoIterator<Item = &'a str>,
    hostfs: &Path,
    harness: &Path,
    ours: &str,
) -> Result<(), String> {
    std::fs::create_dir_all(hostfs).map_err(|e| format!("riscos: {}: {e}", hostfs.display()))?;
    harness_boot(hostfs, ours)?;
    remove_found(hostfs, left)?;
    let program = harness_program(groups.iter().flat_map(|group| &group.scenarios))?;
    std::fs::write(harness, program).map_err(|e| format!("riscos: {}: {e}", harness.display()))?;
    for (name, bytes) in staged {
        let path = hostfs.join(format!("{name},fff"));
        std::fs::write(&path, bytes).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
    }
    for (name, bytes) in library {
        let path = hostfs.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("riscos: {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, bytes).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
    }
    let boot = hostfs.join("!Boot,feb");
    std::fs::write(&boot, ours).map_err(|e| format!("riscos: {}: {e}", boot.display()))
}

/// Removes the harness and `ours`, the `!Boot` that runs it, so the machine
/// boots as it did before.
fn remove_harness(hostfs: &Path, harness: &Path, ours: &str) -> Result<(), String> {
    if let Some(boot) = harness_boot(hostfs, ours)? {
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

/// The harness as BASIC V's tokenised program: every scenario but those
/// marked `ours`, those marked `lasting` after all the others, since every
/// scenario runs on the one machine.
fn harness_program<'a>(
    scenarios: impl IntoIterator<Item = &'a Scenario>,
) -> Result<Vec<u8>, String> {
    let (lasting, others): (Vec<&Scenario>, Vec<&Scenario>) = scenarios
        .into_iter()
        .filter(|scenario| scenario.kind != Kind::Ours)
        .partition(|scenario| scenario.lasting);
    let mut text: Vec<String> = vec![
        "O%=OPENOUT\"HostFS:$.Observed\"".into(),
        "ON ERROR ON ERROR OFF:BPUT#O%,\"! \"+STR$ERR+\" at \"+STR$ERL:CLOSE#O%:PROCdone:END"
            .into(),
        format!(
            "SYS\"OS_Module\",18,\"UtilityModule\" TO ,,,V%:BPUT#O%,\"{VERSION}\"+STR$(INT(VAL(FNv(V%+V%!&14))*100+0.5))"
        ),
    ];
    for scenario in others.into_iter().chain(lasting) {
        scenario_lines(scenario, &mut text)?;
    }
    text.push(obey::run());
    text.extend([
        "CLOSE#O%:PROCdone:END".into(),
        // Saved, as HostFS opens no named pipe for a BPUT.
        "DEF PROCdone:SYS\"OS_File\",10,\"HostFS:$.Done\",&FFD,,PAGE,PAGE+1:ENDPROC".into(),
        "DEF FNh(N%)=\"&\"+RIGHT$(\"0000000\"+STR$~N%,8)".into(),
        "DEF FNr(R$,N%,F%)=\" \"+R$+\"=\"+FNh(N%)".into(),
        // An item an error leaves undefined, `S%` set, gives way to it.
        "DEF FNq(R$,N%,F%,S%):IF S% AND F% AND 1 THEN=\"\" ELSE=FNr(R$,N%,F%)".into(),
        "DEF FNg(F%,B%,N$)=\" \"+N$+\"=\"+STR$(F% DIV B% AND 1)".into(),
        "DEF FNe(N%,F%):IF F% AND 1 THEN=\" error=\"+FNh(!N%) ELSE=\"\"".into(),
        "DEF FNw(K$,M%,B%,F%,S%):IF S% AND F% AND 1 THEN=\"\" ELSE=\" \"+K$+\"=\"+FNh(!M%-B%)"
            .into(),
        "DEF FNb(K$,M%,L%,F%,S%):LOCAL Q%,T$:IF S% AND F% AND 1 THEN=\"\"".into(),
        "T$=\" \"+K$+\"=\":FOR Q%=0 TO L%-1:T$+=RIGHT$(\"0\"+STR$~(M%?Q%),2):NEXT:=T$".into(),
        format!("DEF PROCfill(M%,L%):LOCAL Q%:FOR Q%=0 TO L%-1:M%?Q%=&{UNWRITTEN:X}:NEXT:ENDPROC"),
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
    // Numbered from one, since nothing refers to a line by number: a corpus
    // fills more of BASIC's 65279 lines than tens would leave it.
    let lines = tokenised
        .iter()
        .zip(1..)
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

/// The bytes a `write` puts down on one harness line, a line holding a
/// tokenised statement of BASIC's at most.
const WRITTEN_PER_LINE: usize = 16;

/// The BASIC variable kept value `name` is held in, entering it among
/// `kept` if it is new.
fn kept_variable<'s>(kept: &mut Vec<&'s str>, name: &'s str) -> String {
    let index = kept.iter().position(|k| *k == name).unwrap_or_else(|| {
        kept.push(name);
        kept.len() - 1
    });
    format!("K{index}%")
}

/// The `BPUT` that records `item` of a show, without its line's end.
fn shown_statement(item: &Item, kept: &[&str]) -> Result<String, String> {
    let s0 = u8::from(item.undefined_after_error());
    let key = item.key();
    Ok(match item {
        Item::Register(register) => {
            let variable = VARIABLES[*register as usize];
            format!("BPUT#O%,FNq(\"{key}\",{variable},Z%,{s0});")
        }
        Item::Relative { register, from } => {
            let variable = VARIABLES[*register as usize];
            let base = expression(
                &Value::Kept {
                    name: from.clone(),
                    offset: 0,
                },
                kept,
            )?;
            format!("BPUT#O%,FNq(\"{key}\",{variable}-{base},Z%,{s0});")
        }
        Item::Flag(flag) => format!("BPUT#O%,FNg(Z%,{},\"{}\");", flag.bit(), flag.name()),
        Item::Bytes { at, length } => {
            let address = address(at, kept)?;
            format!("BPUT#O%,FNb(\"{key}\",{address},{length},Z%,{s0});")
        }
        Item::Pointer { at, from } => {
            let address = address(at, kept)?;
            let base = expression(
                &Value::Kept {
                    name: from.clone(),
                    offset: 0,
                },
                kept,
            )?;
            format!("BPUT#O%,FNw(\"{key}\",{address},{base},Z%,{s0});")
        }
    })
}

/// A BASIC expression for where a shown item is.
fn address(at: &At, kept: &[&str]) -> Result<String, String> {
    match at {
        At::Register(register) => Ok(VARIABLES[*register as usize].to_string()),
        At::Kept { name, offset } => expression(
            &Value::Kept {
                name: name.clone(),
                offset: *offset,
            },
            kept,
        ),
    }
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
                let variable = kept_variable(&mut kept, name);
                text.push(format!("{variable}={}", VARIABLES[*register as usize]));
            }
            Step::Block { name, size } => {
                let variable = kept_variable(&mut kept, name);
                text.push(format!(
                    "DIM {variable} {}:PROCfill({variable},{size})",
                    size - 1
                ));
            }
            Step::Write { at, bytes } => {
                let address = expression(at, &kept)?;
                for (part, chunk) in bytes.chunks(WRITTEN_PER_LINE).enumerate() {
                    let mut line = format!("X%={address}");
                    for (index, byte) in chunk.iter().enumerate() {
                        let _ = write!(line, ":X%?{}=&{byte:X}", part * WRITTEN_PER_LINE + index);
                    }
                    text.push(line);
                }
            }
            Step::Show { items } => {
                text.push(format!("BPUT#O%,\"{call}\";"));
                for item in items {
                    text.push(shown_statement(item, &kept)?);
                }
                text.push("BPUT#O%,FNe(A%,Z%)".into());
            }
        }
    }
    Ok(())
}

/// What ended the wait for the harness.
enum Ended {
    /// A byte reached `Done`: whether it was read cleanly.
    Done(bool),
    /// The emulator exited first.
    Exited,
    /// The deadline passed first.
    Overdue,
}

/// Runs `<dir>`'s RPCEmu headless until the harness writes a byte to `Done`,
/// a named pipe here, the emulator exits, or the deadline passes.
fn run_emulator(ctx: &Context, dir: &Path, hostfs: &Path) -> Result<(), String> {
    let done = hostfs.join("Done,ffd");
    let mut mkfifo = Command::new("mkfifo");
    mkfifo.arg(&done);
    ctx.run("riscos: mkfifo", mkfifo)?;
    let outcome = DonePipe::hold(&done)
        .map_err(|e| format!("riscos: {}: {e}", done.display()))
        .and_then(|pipe| emulate(ctx, dir, &pipe));
    let _ = std::fs::remove_file(&done);
    match outcome? {
        Ended::Done(true) => Ok(()),
        Ended::Done(false) => Err(format!("riscos: {} could not be read", done.display())),
        Ended::Exited => Err("riscos: the emulator ended before the harness did".into()),
        Ended::Overdue => Err(format!(
            "riscos: the harness did not finish within {} s",
            DEADLINE.as_secs()
        )),
    }
}

/// Runs the emulator until a byte reaches `pipe`, it exits, or the deadline
/// passes, then stops it and ends the wait on `pipe`.
fn emulate(ctx: &Context, dir: &Path, pipe: &DonePipe) -> Result<Ended, String> {
    let (said, heard) = mpsc::channel();
    let reader = pipe
        .await_byte(said.clone())
        .map_err(|e| format!("riscos: could not start the wait for the harness ({e})"))?;
    let mut emulator = Command::new(dir.join(EMULATOR));
    emulator
        .current_dir(dir)
        .env("QT_QPA_PLATFORM", "offscreen")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let ended = match ctx.children.spawn(&mut emulator) {
        Ok(mut child) => {
            let pid = child.id();
            let waiting = std::thread::Builder::new().spawn(move || {
                let _ = child.wait();
                let _ = said.send(Ended::Exited);
            });
            let ended = match waiting {
                Ok(_) => Ok(heard.recv_timeout(DEADLINE).unwrap_or(Ended::Overdue)),
                Err(e) => Err(format!(
                    "riscos: could not start the wait for the emulator ({e}); pid {pid} was \
                     killed and is not waited for"
                )),
            };
            // The emulator runs until it is stopped, unless it already has.
            if !matches!(ended, Ok(Ended::Exited)) {
                crate::children::kill(pid);
            }
            ctx.children.release(pid);
            ended
        }
        Err(e) => Err(format!("riscos: {}: {e}", dir.join(EMULATOR).display())),
    };
    pipe.release();
    let _ = reader.join();
    ended
}

/// `Done`, held open from before the emulator starts. The harness's open then
/// finds a reader and never waits on this side, the byte it writes stays in
/// the pipe until read, and the pipe never reads as ended. No open waits on
/// another, a rendezvous macOS can lose under load.
struct DonePipe {
    /// Read here, by a wait on a thread of its own.
    reading: Arc<File>,
    /// Written here, never waiting, to end that wait.
    waking: File,
}

impl DonePipe {
    /// The named pipe at `path`, held for reading and for writing; refused
    /// where `path` is not one named pipe at both opens, as a wake written
    /// anywhere else would leave the wait waiting.
    fn hold(path: &Path) -> std::io::Result<Self> {
        // Linux and macOS open a FIFO for reading and writing at once, with
        // no other side yet; the writing end below then finds this reader.
        let reading = OpenOptions::new().read(true).write(true).open(path)?;
        let waking = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        let (read, woken) = (reading.metadata()?, waking.metadata()?);
        if !read.file_type().is_fifo() || (read.dev(), read.ino()) != (woken.dev(), woken.ino()) {
            return Err(std::io::Error::other("not one named pipe"));
        }
        Ok(Self {
            reading: Arc::new(reading),
            waking,
        })
    }

    /// Waits on a thread of its own for the pipe's first byte, whenever it
    /// was written, then says so on `said`.
    fn await_byte(
        &self,
        said: mpsc::Sender<Ended>,
    ) -> std::io::Result<std::thread::JoinHandle<()>> {
        let reading = Arc::clone(&self.reading);
        std::thread::Builder::new().spawn(move || {
            let byte = (&*reading).read_exact(&mut [0; 1]);
            let _ = said.send(Ended::Done(byte.is_ok()));
        })
    }

    /// Ends the wait, whether it has begun or not: the byte waits in the pipe
    /// for it, and a pipe too full to take one holds a byte for it already.
    fn release(&self) {
        let _ = (&self.waking).write(&[0]);
    }
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

/// The bytes the machine wrote into the memory each recorded step shows: an
/// item's bytes parted wherever one was left [`UNWRITTEN`].
fn memory_streams(observed: &str) -> Vec<Vec<u8>> {
    observed
        .lines()
        .flat_map(|line| line.split(' '))
        .filter_map(|token| {
            let (key, hex) = token.split_once('=')?;
            key.contains(':').then_some(())?;
            tairix_riscos_kernel::corpus::hex_bytes(hex)
        })
        .flat_map(|bytes| {
            bytes
                .split(|&b| b == UNWRITTEN)
                .filter(|part| !part.is_empty())
                .map(<[u8]>::to_vec)
                .collect::<Vec<_>>()
        })
        .collect()
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
        let source = if scenario.kind == Kind::Ours {
            &old
        } else {
            observations
        };
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
        let ours = boot(&[]);
        let refused = write_harness(&[], &staged, &[], left(), &hostfs, &harness, &ours)
            .expect_err("the machine's own");
        assert!(refused.contains("move it aside"), "{refused}");
        assert!(!harness.exists());
        assert!(
            remove_harness(&hostfs, &harness, &ours).is_err(),
            "never removed"
        );
        assert_eq!(
            std::fs::read_to_string(hostfs.join("!Boot,feb")).unwrap(),
            "Run Mine\n"
        );
        std::fs::remove_file(hostfs.join("!Boot,feb")).unwrap();
        std::fs::write(hostfs.join(format!("{},fff", obey::RECORDED)), "= stale\n").unwrap();
        write_harness(&[], &staged, &[], left(), &hostfs, &harness, &ours).unwrap();
        write_harness(&[], &staged, &[], left(), &hostfs, &harness, &ours).unwrap();
        assert!(find(&hostfs, "!Boot").is_some() && harness.exists());
        assert!(
            find(&hostfs, obey::RECORDED).is_none(),
            "an old run's answers"
        );
        assert_eq!(
            std::fs::read_to_string(hostfs.join(format!("{},fff", obey::STAGED))).unwrap(),
            cases
        );
        remove_harness(&hostfs, &harness, &ours).unwrap();
        assert!(find(&hostfs, "!Boot").is_none() && !harness.exists());
    }

    #[test]
    fn the_emulator_runs_until_the_harness_writes_to_done() {
        let (scratch, ctx) = emulated("printf x > hostfs/Done,ffd\nexec sleep 60");
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

    /// A held `Done` in a scratch directory of its own, and its path.
    fn held() -> (tairix_fuzzseed::scratch::Scratch, PathBuf, DonePipe) {
        let scratch =
            tairix_fuzzseed::scratch::Scratch::under(&std::env::temp_dir(), "riscos-done");
        let path = scratch.path().join("Done,ffd");
        let made = Command::new("mkfifo").arg(&path).status();
        assert!(made.is_ok_and(|status| status.success()), "a named pipe");
        let pipe = DonePipe::hold(&path).expect("held");
        (scratch, path, pipe)
    }

    /// `path` opened as the harness opens it, refused rather than waited on
    /// where no reader holds it.
    fn as_the_harness(path: &Path) -> File {
        OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .expect("held, so the harness never waits for a reader")
    }

    /// Whether the wait on `pipe` ended on a byte read cleanly, within a
    /// minute.
    fn ended_on_a_byte(pipe: &DonePipe) -> bool {
        let (said, heard) = mpsc::channel();
        let reader = pipe.await_byte(said).expect("a wait");
        let ended = matches!(
            heard.recv_timeout(crate::process::MINUTE),
            Ok(Ended::Done(true))
        );
        pipe.release();
        reader.join().is_ok() && ended
    }

    /// The harness finishing before the wait has begun is still seen: the byte
    /// waits in the pipe, so nothing hangs on when the waiting thread runs.
    #[test]
    fn a_byte_written_before_the_wait_begins_ends_it() {
        let (_scratch, path, pipe) = held();
        as_the_harness(&path).write_all(&[0]).expect("written");
        assert!(ended_on_a_byte(&pipe));
    }

    /// An emulator that ends before the waiting thread has run still has its
    /// wait ended, rather than leaving that thread waiting for good.
    #[test]
    fn a_release_before_the_wait_begins_ends_it() {
        let (_scratch, _path, pipe) = held();
        pipe.release();
        assert!(ended_on_a_byte(&pipe));
    }

    /// Only a byte ends the wait: an open and close that wrote nothing does
    /// not, and the pipe does not read as ended once it is closed.
    #[test]
    fn an_open_and_close_writing_nothing_leaves_the_wait_under_way() {
        let (_scratch, path, pipe) = held();
        drop(as_the_harness(&path));
        let (said, heard) = mpsc::channel();
        let reader = pipe.await_byte(said).expect("a wait");
        assert!(
            heard.recv_timeout(crate::tests::NO_ANSWER).is_err(),
            "still waiting"
        );
        pipe.release();
        assert!(matches!(
            heard.recv_timeout(crate::process::MINUTE),
            Ok(Ended::Done(true))
        ));
        reader.join().expect("the wait ended");
    }

    /// A `Done` that is not a named pipe is refused rather than held, as no
    /// wake written to it could end the wait.
    #[test]
    fn a_done_that_is_no_named_pipe_is_refused() {
        let scratch =
            tairix_fuzzseed::scratch::Scratch::under(&std::env::temp_dir(), "riscos-done");
        let path = scratch.path().join("Done,ffd");
        std::fs::write(&path, b"").expect("a plain file");
        let refused = DonePipe::hold(&path).err().map(|e| e.to_string());
        assert_eq!(refused.as_deref(), Some("not one named pipe"));
    }

    /// The harness's last act is the one the command waits on.
    #[test]
    fn the_harness_ends_by_writing_a_byte_to_done() {
        use tairix_openbasic::list::{Lister, Options};

        let image = harness_program(std::iter::empty()).unwrap();
        let mut lister = Lister::new(Dialect::BasicV, Options::new(0));
        let mut out = Vec::new();
        for line in program::lines(&image) {
            let line = line.unwrap();
            lister.line(line.number, line.text, &mut out).unwrap();
        }
        let text = String::from_utf8(out).unwrap();
        let done = text
            .lines()
            .find(|line| line.contains("DEF PROCdone"))
            .expect("PROCdone");
        assert!(
            done.contains("SYS\"OS_File\",10,\"HostFS:$.Done\",&FFD,,PAGE,PAGE+1"),
            "{done}"
        );
    }

    #[test]
    fn the_harness_tokenises_and_every_scenario_step_becomes_a_line() {
        let scenarios = parse_scenarios(
            "scenario one\ncall XOS_DynamicArea r0=0 r1=-1 r8=\"Corpus\"\nkeep r1 area\ncall XOS_DynamicArea r0=2 r1=$area+&10\nshow r0 r2 v\nblock buffer 8\nwrite $buffer+1 w=&04030201 b=5 s=\"ab\"\ncall XOS_ConvertHex2 r0=&41 r1=$buffer r2=8\nshow r0-buffer r1-area $buffer+&1:3 r0:2\n",
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
                "BPUT#O%,\"2\";",
                "BPUT#O%,FNq(\"r0\",A%,Z%,1);",
                "BPUT#O%,FNq(\"r2\",C%,Z%,0);",
                "BPUT#O%,FNg(Z%,1,\"v\");",
                "BPUT#O%,FNe(A%,Z%)",
                "DIM K1% 7:PROCfill(K1%,8)",
                "X%=K1%+&1:X%?0=&1:X%?1=&2:X%?2=&3:X%?3=&4:X%?4=&5:X%?5=&61:X%?6=&62",
                "SYS\"XOS_ConvertHex2\",&41,K1%,&8 TO A%,B%,C%,D%,E%,F%,G%,H%,I%,J%;Z%",
                "BPUT#O%,\"3\";",
                "BPUT#O%,FNq(\"r0-buffer\",A%-K1%,Z%,1);",
                "BPUT#O%,FNq(\"r1-area\",B%-K0%,Z%,0);",
                "BPUT#O%,FNb(\"$buffer+&1:3\",K1%+&1,3,Z%,0);",
                "BPUT#O%,FNb(\"r0:2\",A%,2,Z%,1);",
                "BPUT#O%,FNe(A%,Z%)",
            ]
        );
        let pointed =
            parse_scenarios("scenario two\nblock out 8\ncall X\nshow $out+&4@out r2@out\n")
                .unwrap();
        let mut text = Vec::new();
        scenario_lines(&pointed[0], &mut text).unwrap();
        assert_eq!(
            text[4..6],
            [
                "BPUT#O%,FNw(\"$out+&4@out\",K0%+&4,K0%,Z%,0);",
                "BPUT#O%,FNw(\"r2@out\",C%,K0%,Z%,1);",
            ]
        );
        let image = harness_program(&scenarios).unwrap();
        assert!(
            program::lines(&image).all(|line| line.is_ok()),
            "a well-formed program"
        );
    }

    #[test]
    fn a_scenario_whose_fixture_is_ours_is_left_out_of_the_harness() {
        let scenarios = parse_scenarios(
            "scenario theirs\ncall X\nshow v\nscenario mine ours\ncall Y\nshow v\n",
        )
        .unwrap();
        let image = harness_program(&scenarios).unwrap();
        let holds = |name: &[u8]| image.windows(name.len()).any(|w| w == name);
        assert!(holds(b"scenario theirs"));
        assert!(!holds(b"scenario mine"), "never run on genuine RISC OS");
    }

    #[test]
    fn a_lasting_scenario_runs_after_every_other() {
        let scenarios = parse_scenarios(
            "scenario stays lasting\ncall X\nscenario first\ncall Y\nscenario second\ncall Z\n",
        )
        .unwrap();
        let image = harness_program(&scenarios).unwrap();
        let at = |name: &[u8]| image.windows(name.len()).position(|w| w == name);
        let (first, second, stays) = (
            at(b"scenario first"),
            at(b"scenario second"),
            at(b"scenario stays"),
        );
        assert!(
            first < second && second < stays && first.is_some(),
            "{first:?} {second:?} {stays:?}"
        );
    }

    #[test]
    fn a_write_longer_than_a_line_holds_runs_on_to_the_next() {
        let bytes = (0..20).fold(String::new(), |mut text, n| {
            let _ = write!(text, " b={n}");
            text
        });
        let scenarios =
            parse_scenarios(&format!("scenario long\nblock b 32\nwrite $b{bytes}\n")).unwrap();
        let mut text = Vec::new();
        scenario_lines(&scenarios[0], &mut text).unwrap();
        assert_eq!(text.len(), 4, "the scenario, the block and two lines");
        assert!(text[3].starts_with("X%=K0%:X%?16=&10:"), "{}", text[3]);
    }

    #[test]
    fn shown_memory_is_what_lies_between_the_unwritten_bytes() {
        assert_eq!(
            memory_streams(
                "1 r0=&04030201 $b+&4:3=414243 v=0\n2 r1:6=0D0AEEEE00EE error=&000001E4\n"
            ),
            [b"ABC".to_vec(), vec![13, 10], vec![0]]
        );
    }

    /// The streams of `recorded`: its text, its registers' bytes and the
    /// memory it shows.
    fn streams(recorded: &str) -> Vec<Vec<u8>> {
        let mut streams = vec![recorded.as_bytes().to_vec()];
        streams.extend(register_streams(recorded));
        streams.extend(memory_streams(recorded));
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
