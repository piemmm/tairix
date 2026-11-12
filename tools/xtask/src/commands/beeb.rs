//! `cargo xtask beeb` implementation: the BBC Micro emulator's conformance
//! suites and generated artefacts (plans/BEEB.md).
//!
//! - `cargo xtask beeb --vectors <dir>` runs every case of the `SingleStepTests`
//!   65x02 `6502/v1` suite held in `<dir>` (one `<opcode>.json` per opcode,
//!   commit [`SUITE_COMMIT`]) through the emulator's CPU and fails naming each
//!   opcode that departs from it. Developer-run only: the suite is a gigabyte
//!   and is not in the tree.
//! - `--write-sample` additionally rewrites the committed sample the crate's
//!   own tests replay in `ci`: evenly spaced cases of every opcode, and more of
//!   the opcodes decimal mode changes.

use std::ffi::OsString;
use std::path::Path;

use tairix_beeb::cpu::isa::{Mnemonic, OPCODES};
use tairix_beeb::cpu::vectors::{self, Cycle, Runner, Snapshot, Vector};
use tairix_beeb::cpu::{flag, Registers};

use super::json::{self, Json};
use crate::Context;

/// The suite revision the committed sample was drawn from.
pub const SUITE_COMMIT: &str = "2f6980a2d95757486c7bee24355c360e40e2a224";

/// Where the committed sample lives.
const SAMPLE: &str = "userland/apps/beeb/tests/vectors/sst6502.bin";

/// Evenly spaced cases kept of every opcode.
const PER_OPCODE: usize = 16;

/// Further decimal-mode cases kept of every opcode decimal mode changes.
const DECIMAL_EXTRA: usize = 48;

/// How many failing opcodes a report names before summarising the rest.
const REPORTED: usize = 24;

fn usage() -> String {
    "beeb: usage: cargo xtask beeb --vectors <dir> [--write-sample]".to_string()
}

pub fn run(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let args: Vec<&str> = args
        .iter()
        .map(|arg| arg.to_str().ok_or_else(usage))
        .collect::<Result<_, _>>()?;
    match args.as_slice() {
        ["--vectors", dir] => vectors(ctx, Path::new(dir), false),
        ["--vectors", dir, "--write-sample"] => vectors(ctx, Path::new(dir), true),
        _ => Err(usage()),
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
            .map_err(|e| format!("beeb: cannot read {}: {e}", path.display()))?;
        let document = json::parse(&text).map_err(|e| format!("beeb: {}: {e}", path.display()))?;
        let cases = document
            .as_array()
            .ok_or_else(|| format!("beeb: {}: not an array of cases", path.display()))?;
        let mut decoded = Vec::with_capacity(cases.len());
        for case in cases {
            decoded.push(parse_case(case).map_err(|e| format!("beeb: {}: {e}", path.display()))?);
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
    noteln!("xtask: [beeb --vectors] {total} cases, suite {SUITE_COMMIT}");
    if !failures.is_empty() {
        let shown = failures.len().min(REPORTED);
        return Err(format!(
            "beeb: {} opcodes depart from the suite:\n  {}{}",
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
        let bytes = vectors::encode(&sample)
            .map_err(|_| "beeb: a case touches more than 255 addresses or cycles".to_string())?;
        let path = ctx.workspace_root.join(SAMPLE);
        std::fs::write(&path, bytes)
            .map_err(|e| format!("beeb: cannot write {}: {e}", path.display()))?;
        noteln!(
            "xtask: [beeb --write-sample] {} cases to {SAMPLE}",
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
    use super::{pick, Registers, Snapshot, Vector, PER_OPCODE};

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
