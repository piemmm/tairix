//! `cargo xtask openmicro` implementation: the BBC Micro emulator's conformance
//! suites and generated artefacts (plans/BEEB.md).
//!
//! - `cargo xtask openmicro --vectors <dir>` runs every case of the `SingleStepTests`
//!   65x02 `6502/v1` suite held in `<dir>` (one `<opcode>.json` per opcode,
//!   commit [`SUITE_COMMIT`]) through the emulator's CPU and fails naming each
//!   opcode that departs from it. Developer-run only: the suite is a gigabyte
//!   and is not in the tree.
//! - `--write-sample` additionally rewrites the committed sample the crate's
//!   own tests replay in `ci`: evenly spaced cases of every opcode, and more of
//!   the opcodes decimal mode changes.
//! - `cargo xtask openmicro --write-roms` rebuilds every clean-room ROM image
//!   `Run` carries into the crate's `firmware/`; the `openmicro-roms` static gate
//!   ([`check_roms`]) refuses a committed image the tree does not build.
//! - `cargo xtask openmicro --reference <dir>` records the corpus's fixtures from
//!   the original ROMs a developer holds in `<dir>` ([`mod@reference`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tairix_openmicro::cpu::isa::{Mnemonic, OPCODES};
use tairix_openmicro::cpu::vectors::{self, Cycle, Runner, Snapshot, Vector};
use tairix_openmicro::cpu::{flag, Registers};
use tairix_openmicro::rom::SHIPPED;

use super::json::{self, Json};
use crate::Context;

mod reference;

/// The suite revision the committed sample was drawn from.
pub const SUITE_COMMIT: &str = "2f6980a2d95757486c7bee24355c360e40e2a224";

/// Where the committed sample lives.
const SAMPLE: &str = "userland/apps/openmicro/tests/vectors/sst6502.bin";

/// The crate whose `firmware/` holds the shipped ROM images.
const BUNDLE: &str = "userland/apps/openmicro";

/// Evenly spaced cases kept of every opcode.
const PER_OPCODE: usize = 16;

/// Further decimal-mode cases kept of every opcode decimal mode changes.
const DECIMAL_EXTRA: usize = 48;

/// How many failing opcodes a report names before summarising the rest.
const REPORTED: usize = 24;

fn usage() -> String {
    "OpenMicro: usage: cargo xtask openmicro --vectors <dir> [--write-sample] | --write-roms | --reference <dir>"
        .to_string()
}

pub fn run(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let args: Vec<&str> = args
        .iter()
        .map(|arg| arg.to_str().ok_or_else(usage))
        .collect::<Result<_, _>>()?;
    match args.as_slice() {
        ["--vectors", dir] => vectors(ctx, Path::new(dir), false),
        ["--vectors", dir, "--write-sample"] => vectors(ctx, Path::new(dir), true),
        ["--write-roms"] => write_roms(ctx),
        ["--reference", dir] => reference::run(ctx, Path::new(dir)),
        _ => Err(usage()),
    }
}

/// The crate's `firmware/`.
fn firmware(ctx: &Context) -> PathBuf {
    ctx.workspace_root
        .join(BUNDLE)
        .join(tairix_openmicro::firmware::DIR)
}

/// Every shipped image as this tree builds it.
fn built_roms() -> Result<Vec<(&'static str, Vec<u8>)>, String> {
    SHIPPED
        .iter()
        .map(|&(name, build)| {
            build()
                .map(|image| (name, image.bytes))
                .map_err(|e| format!("OpenMicro: {name} does not assemble: {e:?}"))
        })
        .collect()
}

fn write_roms(ctx: &Context) -> Result<(), String> {
    let dir = firmware(ctx);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("OpenMicro: cannot create {}: {e}", dir.display()))?;
    let roms = built_roms()?;
    for (name, bytes) in &roms {
        let path = dir.join(name);
        std::fs::write(&path, bytes)
            .map_err(|e| format!("OpenMicro: cannot write {}: {e}", path.display()))?;
    }
    noteln!(
        "xtask: [openmicro --write-roms] {} image(s) to {BUNDLE}/{}",
        roms.len(),
        tairix_openmicro::firmware::DIR
    );
    Ok(())
}

/// The `openmicro-roms` static gate: each committed image is exactly the one this
/// tree builds, and nothing named as an image is committed that the bundle
/// does not ship.
pub fn check_roms(ctx: &Context) -> Result<(), String> {
    let dir = firmware(ctx);
    noteln!("xtask: [openmicro-roms] {}", dir.display());
    let mut faults = Vec::new();
    let roms = built_roms()?;
    for (name, bytes) in &roms {
        match std::fs::read(dir.join(name)) {
            Ok(committed) if committed == *bytes => {}
            Ok(_) => faults.push(format!("{name} is not the image the tree builds")),
            Err(e) => faults.push(format!("{name}: {e}")),
        }
    }
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("openmicro-roms: cannot list {}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|e| format!("openmicro-roms: cannot list {}: {e}", dir.display()))?
            .path();
        let shipped = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| roms.iter().any(|(rom, _)| *rom == name));
        let image = path.extension().is_some_and(|extension| {
            roms.iter()
                .any(|(rom, _)| Path::new(rom).extension() == Some(extension))
        });
        if image && !shipped {
            faults.push(format!(
                "{} is not an image the bundle ships",
                path.display()
            ));
        }
    }
    if faults.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "openmicro-roms: {}; run `cargo xtask openmicro --write-roms`",
            faults.join("; ")
        ))
    }
}

fn vectors(ctx: &Context, dir: &Path, write_sample: bool) -> Result<(), String> {
    let mut runner = Runner::new();
    let mut sample = Vec::new();
    let mut failures = Vec::new();
    let mut total = 0usize;
    for code in 0..=u8::MAX {
        let path = dir.join(format!("{code:02x}.json"));
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("OpenMicro: cannot read {}: {e}", path.display()))?;
        let document =
            json::parse(&text).map_err(|e| format!("OpenMicro: {}: {e}", path.display()))?;
        let cases = document
            .as_array()
            .ok_or_else(|| format!("OpenMicro: {}: not an array of cases", path.display()))?;
        let mut decoded = Vec::with_capacity(cases.len());
        for case in cases {
            decoded
                .push(parse_case(case).map_err(|e| format!("OpenMicro: {}: {e}", path.display()))?);
        }
        let mut failed = 0usize;
        let mut first = None;
        for (index, vector) in decoded.iter().enumerate() {
            if let Err(mismatch) = runner.check(vector) {
                failed += 1;
                first.get_or_insert((index, mismatch));
            }
        }
        total += decoded.len();
        if let Some((index, mismatch)) = first {
            let name = OPCODES[usize::from(code)].mnemonic.name();
            failures.push(format!(
                "&{code:02X} {name}: {failed} of {} failed; case {index}: {mismatch}",
                decoded.len()
            ));
        }
        if write_sample {
            sample.extend(pick(code, &decoded));
        }
    }
    noteln!("xtask: [openmicro --vectors] {total} cases, suite {SUITE_COMMIT}");
    if !failures.is_empty() {
        let shown = failures.len().min(REPORTED);
        return Err(format!(
            "OpenMicro: {} opcodes depart from the suite:\n  {}{}",
            failures.len(),
            failures[..shown].join("\n  "),
            if failures.len() > shown {
                "\n  …"
            } else {
                ""
            }
        ));
    }
    if write_sample {
        let bytes = vectors::encode(&sample).map_err(|_| {
            "OpenMicro: a case touches more than 255 addresses or cycles".to_string()
        })?;
        let path = ctx.workspace_root.join(SAMPLE);
        std::fs::write(&path, bytes)
            .map_err(|e| format!("OpenMicro: cannot write {}: {e}", path.display()))?;
        noteln!(
            "xtask: [openmicro --write-sample] {} cases to {SAMPLE}",
            sample.len()
        );
    }
    Ok(())
}

/// The cases of one opcode the sample keeps: evenly spaced ones, each once,
/// and for an opcode decimal mode changes, further cases run in decimal mode.
fn pick(code: u8, cases: &[Vector]) -> Vec<Vector> {
    let count = PER_OPCODE.min(cases.len());
    let spaced: Vec<usize> = (0..count).map(|n| n * cases.len() / count).collect();
    let mut picked: Vec<Vector> = spaced.iter().map(|&i| cases[i].clone()).collect();
    if decimal_sensitive(OPCODES[usize::from(code)].mnemonic) {
        picked.extend(
            cases
                .iter()
                .enumerate()
                .filter(|(i, v)| !spaced.contains(i) && v.before.registers.p & flag::D != 0)
                .take(DECIMAL_EXTRA)
                .map(|(_, v)| v.clone()),
        );
    }
    picked
}

fn decimal_sensitive(mnemonic: Mnemonic) -> bool {
    matches!(
        mnemonic,
        Mnemonic::Adc | Mnemonic::Sbc | Mnemonic::Rra | Mnemonic::Isc | Mnemonic::Arr
    )
}

fn parse_case(case: &Json) -> Result<Vector, String> {
    let before = parse_snapshot(case.get("initial").ok_or("a case has no `initial`")?)?;
    let after = parse_snapshot(case.get("final").ok_or("a case has no `final`")?)?;
    let cycles = case
        .get("cycles")
        .and_then(Json::as_array)
        .ok_or("a case has no `cycles`")?
        .iter()
        .map(|cycle| {
            let [addr, data, kind] = cycle.as_array().ok_or("a cycle is not an array")? else {
                return Err("a cycle is not [address, data, kind]".to_string());
            };
            let write = match kind.as_str() {
                Some("read") => false,
                Some("write") => true,
                _ => return Err("a cycle is neither a read nor a write".to_string()),
            };
            Ok(Cycle {
                addr: number(addr)?,
                data: number(data)?,
                write,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(Vector {
        before,
        after,
        cycles,
    })
}

fn parse_snapshot(state: &Json) -> Result<Snapshot, String> {
    let field = |key: &str| {
        state
            .get(key)
            .ok_or_else(|| format!("a state has no `{key}`"))
    };
    let registers = Registers {
        pc: number(field("pc")?)?,
        s: number(field("s")?)?,
        a: number(field("a")?)?,
        x: number(field("x")?)?,
        y: number(field("y")?)?,
        p: number(field("p")?)?,
    };
    let ram = field("ram")?
        .as_array()
        .ok_or("`ram` is not an array")?
        .iter()
        .map(|entry| {
            let [addr, data] = entry.as_array().ok_or("a ram entry is not an array")? else {
                return Err("a ram entry is not [address, data]".to_string());
            };
            Ok((number(addr)?, number(data)?))
        })
        .collect::<Result<_, _>>()?;
    Ok(Snapshot { registers, ram })
}

fn number<T: TryFrom<u64>>(value: &Json) -> Result<T, String> {
    value
        .as_u64()
        .and_then(|n| T::try_from(n).ok())
        .ok_or_else(|| "a number is out of range".to_string())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::{check_roms, firmware, pick, write_roms, Registers, Snapshot, Vector, PER_OPCODE};
    use crate::children::Children;
    use crate::Context;

    fn rooted_at(root: PathBuf) -> Context {
        Context {
            workspace_root: root,
            cargo: OsString::from("true"),
            children: Arc::new(Children::new(None)),
        }
    }

    #[test]
    fn the_committed_images_are_the_ones_the_tree_builds() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("the workspace root");
        assert_eq!(check_roms(&rooted_at(root.to_path_buf())), Ok(()));
    }

    #[test]
    fn the_gate_refuses_a_drifted_image_and_one_the_bundle_does_not_ship() {
        let scratch = tairix_fuzzseed::scratch::Scratch::under(
            &std::env::temp_dir(),
            "tairix-openmicro-roms",
        );
        let ctx = rooted_at(scratch.path().to_path_buf());
        write_roms(&ctx).expect("written");
        assert_eq!(check_roms(&ctx), Ok(()));
        let dir = firmware(&ctx);
        std::fs::write(dir.join(tairix_openmicro::firmware::MOS), [0; 16]).expect("drifted");
        std::fs::write(dir.join("old.rom"), [0; 16]).expect("left behind");
        let refused = check_roms(&ctx).expect_err("refused");
        assert!(
            refused.contains("mos.rom is not the image the tree builds"),
            "{refused}"
        );
        assert!(
            refused.contains("old.rom is not an image the bundle ships"),
            "{refused}"
        );
    }

    fn case(pc: u16) -> Vector {
        let state = Snapshot {
            registers: Registers {
                a: 0,
                x: 0,
                y: 0,
                s: 0xFD,
                p: 0,
                pc,
            },
            ram: Vec::new(),
        };
        Vector {
            before: state.clone(),
            after: state,
            cycles: Vec::new(),
        }
    }

    /// The regression: the picker indexed its cases unconditionally, so an
    /// empty opcode file panicked and a short one gave cases twice.
    #[test]
    fn the_sample_keeps_each_case_at_most_once_and_none_of_an_empty_opcode() {
        assert!(pick(0xEA, &[]).is_empty());
        let few: Vec<Vector> = (0..5).map(case).collect();
        let picked: Vec<u16> = pick(0xEA, &few)
            .iter()
            .map(|v| v.before.registers.pc)
            .collect();
        assert_eq!(picked, [0, 1, 2, 3, 4]);
        let many: Vec<Vector> = (0..1000).map(case).collect();
        let picked = pick(0xEA, &many);
        assert_eq!(picked.len(), PER_OPCODE);
        assert!(picked
            .windows(2)
            .all(|pair| pair[0].before.registers.pc < pair[1].before.registers.pc));
    }
}
