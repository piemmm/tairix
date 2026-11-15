//! `cargo xtask riscos --reference <dir>`: the RISC OS emulator's reference
//! harness (`plans/RISCOS-EMULATOR.md` §17).
//!
//! `<dir>` is a genuine RISC OS a developer holds, laid out as RPCEmu lays
//! its own out: `roms/` the ROM images, `hostfs/` the directory the machine
//! boots from. The command writes the harness there: a BASIC program, built
//! with `lib/bbcbasic`, that runs every corpus scenario's calls and records
//! what each answered, and a `!Boot` that runs it. Where `<dir>` holds RPCEmu
//! it is run headless until the harness is done: the harness ends by opening
//! `Done`, which here is a named pipe the command waits on. Otherwise the
//! harness is left for the developer to run, and a later run imports what it
//! recorded. Each recorded observation becomes its scenario's fixture, but
//! for a scenario marked `ours`.
//!
//! No fixture may carry the ROM's bytes: a run of [`ROM_RUN`] bytes of a ROM
//! image in what was recorded refuses the whole recording, unless the
//! corpus's own scenario files hold that run.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use tairix_bbcbasic::program::{self, Line};
use tairix_bbcbasic::tokenise::{tokenise, Start};
use tairix_bbcbasic::Dialect;
use tairix_riscos_kernel::corpus::{
    format_observations, parse_observations, parse_scenarios, Observation, Scenario, Step, Value,
    REGISTERS,
};

use crate::Context;

/// Where each crate keeps its corpus, from the workspace's root.
const CORPUS: &str = "userland/apps/riscos/kernel/tests/corpus";
/// The emulator `<dir>` may hold, run headless.
const EMULATOR: &str = "rpcemu-recompiler";
/// The longest a recording may take.
const DEADLINE: Duration = Duration::from_secs(300);
/// The shortest run of a ROM's bytes a fixture may not carry.
const ROM_RUN: usize = 12;
/// The comment each fixtures file opens with.
const HEADER: [&str; 2] = [
    "Recorded by `cargo xtask riscos --reference` from genuine RISC OS 5, but for",
    "a scenario marked `ours`, whose observation is this emulator's own.",
];
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
    let hostfs = dir.join("hostfs");
    let harness = hostfs.join("Harness,ffb");
    let observed = if dir.join(EMULATOR).exists() {
        write_harness(&groups, &hostfs, &harness)?;
        run_emulator(dir, &hostfs)?;
        let observed =
            find(&hostfs, "Observed").ok_or("riscos: the harness ended without its recording")?;
        std::fs::read(&observed)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|e| format!("riscos: {}: {e}", observed.display()))?
    } else if let Some(text) = recording(&hostfs, &harness)? {
        text
    } else {
        write_harness(&groups, &hostfs, &harness)?;
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
    let ours: Vec<&str> = groups.iter().map(|group| group.source.as_str()).collect();
    refuse_rom_runs(dir, &observed, &ours)?;
    for group in &groups {
        write_fixtures(group, &observations)?;
    }
    outln!("riscos: {} observations recorded", observations.len())
}

/// What the harness recorded since it was last written, if it has finished.
fn recording(hostfs: &Path, harness: &Path) -> Result<Option<String>, String> {
    let written = std::fs::metadata(harness).and_then(|m| m.modified()).ok();
    let done = find(hostfs, "Done");
    let observed = find(hostfs, "Observed");
    let (Some(done), Some(observed)) = (done, observed) else {
        return Ok(None);
    };
    let fresh = |path: &Path| {
        let modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        written.is_none_or(|written| modified >= written)
    };
    if !fresh(&done) || !fresh(&observed) {
        return Ok(None);
    }
    std::fs::read(&observed)
        .map(|bytes| Some(String::from_utf8_lossy(&bytes).into_owned()))
        .map_err(|e| format!("riscos: {}: {e}", observed.display()))
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

fn write_harness(groups: &[Group], hostfs: &Path, harness: &Path) -> Result<(), String> {
    std::fs::create_dir_all(hostfs).map_err(|e| format!("riscos: {}: {e}", hostfs.display()))?;
    for stale in ["Done", "Observed"] {
        if let Some(path) = find(hostfs, stale) {
            std::fs::remove_file(&path).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
        }
    }
    let program = harness_program(groups.iter().flat_map(|group| &group.scenarios))?;
    std::fs::write(harness, program).map_err(|e| format!("riscos: {}: {e}", harness.display()))?;
    let boot = hostfs.join("!Boot,feb");
    std::fs::write(
        &boot,
        "| The TAIRiX reference harness.\nRun HostFS:$.Harness\n",
    )
    .map_err(|e| format!("riscos: {}: {e}", boot.display()))
}

/// The harness as BASIC V's tokenised program.
fn harness_program<'a>(
    scenarios: impl IntoIterator<Item = &'a Scenario>,
) -> Result<Vec<u8>, String> {
    let mut text: Vec<String> = vec![
        "O%=OPENOUT\"HostFS:$.Observed\"".into(),
        "ON ERROR ON ERROR OFF:BPUT#O%,\"! \"+STR$ERR+\" at \"+STR$ERL:CLOSE#O%:PROCdone:END"
            .into(),
    ];
    for scenario in scenarios {
        scenario_lines(scenario, &mut text)?;
    }
    text.extend([
        "CLOSE#O%:PROCdone:END".into(),
        "DEF PROCdone:LOCAL D%:D%=OPENOUT\"HostFS:$.Done\":CLOSE#D%:ENDPROC".into(),
        "DEF FNh(N%)=\"&\"+RIGHT$(\"0000000\"+STR$~N%,8)".into(),
        "DEF FNr(R$,N%,F%)=\" \"+R$+\"=\"+FNh(N%)".into(),
        "DEF FNz(N%,F%):IF F% AND 1 THEN=\"\" ELSE=FNr(\"r0\",N%,F%)".into(),
        "DEF FNg(F%,B%,N$)=\" \"+N$+\"=\"+STR$(F% DIV B% AND 1)".into(),
        "DEF FNe(N%,F%):IF F% AND 1 THEN=\" error=\"+FNh(!N%) ELSE=\"\"".into(),
    ]);
    let mut tokenised = Vec::with_capacity(text.len());
    for line in &text {
        let bytes = tokenise(Dialect::BasicV, line.as_bytes(), Start::Statement)
            .map_err(|e| format!("riscos: the harness line {line:?}: {e:?}"))?
            .bytes;
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

/// Runs `<dir>`'s RPCEmu headless until the harness opens `Done`, a named
/// pipe here, or the deadline passes.
fn run_emulator(dir: &Path, hostfs: &Path) -> Result<(), String> {
    let done = hostfs.join("Done,ffd");
    let made = Command::new("mkfifo")
        .arg(&done)
        .status()
        .map_err(|e| format!("riscos: mkfifo: {e}"))?;
    if !made.success() {
        return Err(format!("riscos: mkfifo {} failed", done.display()));
    }
    let (said, heard) = mpsc::channel();
    let pipe = done.clone();
    // Opening the pipe blocks until the harness opens it; its close ends the
    // read.
    let reader = std::thread::spawn(move || {
        let ended =
            std::fs::File::open(&pipe).and_then(|mut file| file.read_to_end(&mut Vec::new()));
        let _ = said.send(ended.is_ok());
    });
    let child = Command::new(dir.join(EMULATOR))
        .current_dir(dir)
        .env("QT_QPA_PLATFORM", "offscreen")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let outcome = match child {
        Ok(mut child) => {
            let outcome = heard.recv_timeout(DEADLINE);
            let _ = child.kill();
            let _ = child.wait();
            outcome
        }
        Err(e) => {
            let _ = std::fs::OpenOptions::new().write(true).open(&done);
            let _ = reader.join();
            let _ = std::fs::remove_file(&done);
            return Err(format!("riscos: {}: {e}", dir.join(EMULATOR).display()));
        }
    };
    if outcome.is_err() {
        // Nothing opened the pipe: open it to free the reader.
        let _ = std::fs::OpenOptions::new().write(true).open(&done);
    }
    let _ = reader.join();
    let _ = std::fs::remove_file(&done);
    match outcome {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("riscos: {} could not be read", done.display())),
        Err(_) => Err(format!(
            "riscos: the harness did not finish within {} s",
            DEADLINE.as_secs()
        )),
    }
}

/// Refuses a recording that carries a run of [`ROM_RUN`] bytes of any of
/// `<dir>`'s ROM images that none of `ours` holds.
fn refuse_rom_runs(dir: &Path, observed: &str, ours: &[&str]) -> Result<(), String> {
    let roms = dir.join("roms");
    let held: HashSet<&[u8]> = ours
        .iter()
        .flat_map(|text| text.as_bytes().windows(ROM_RUN))
        .collect();
    let wanted: HashSet<&[u8]> = observed
        .as_bytes()
        .windows(ROM_RUN)
        .filter(|run| !held.contains(run))
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(&roms).map_err(|e| format!("riscos: {}: {e}", roms.display()))?;
    let mut checked = 0;
    for entry in entries.filter_map(Result::ok) {
        let image = std::fs::read(entry.path())
            .map_err(|e| format!("riscos: {}: {e}", entry.path().display()))?;
        if let Some(run) = image.windows(ROM_RUN).find(|run| wanted.contains(run)) {
            return Err(format!(
                "riscos: refused: the recording carries {:?} from {}",
                String::from_utf8_lossy(run),
                entry.path().display()
            ));
        }
        checked += 1;
    }
    if checked == 0 {
        return Err(format!(
            "riscos: no ROM image in {} to check the recording against",
            roms.display()
        ));
    }
    Ok(())
}

/// Rewrites a group's fixtures from what was recorded, keeping those of its
/// scenarios marked `ours`.
fn write_fixtures(group: &Group, observations: &[Observation]) -> Result<(), String> {
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
        fixtures.push(fixture);
    }
    std::fs::write(&group.fixtures, format_observations(&HEADER, &fixtures))
        .map_err(|e| format!("riscos: {}: {e}", group.fixtures.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_recording_carrying_a_roms_bytes_is_refused() {
        let dir = std::env::temp_dir().join(format!("riscos-reference-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("roms")).unwrap();
        std::fs::write(dir.join("roms/riscos"), b"....Unknown dynamic area....").unwrap();
        assert!(refuse_rom_runs(&dir, "scenario a\n1 error=&00000105\n", &[]).is_ok());
        assert!(refuse_rom_runs(&dir, "scenario a\n1 Unknown dynamic area\n", &[]).is_err());
        assert!(
            refuse_rom_runs(
                &dir,
                "scenario Unknown-dynamic-area\n",
                &["scenario Unknown-dynamic-area"]
            )
            .is_ok(),
            "a run the corpus's own text holds"
        );
        std::fs::remove_file(dir.join("roms/riscos")).unwrap();
        assert!(
            refuse_rom_runs(&dir, "scenario a\n1 error=&00000105\n", &[]).is_err(),
            "nothing to check against"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
