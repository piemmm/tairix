//! The recorded corpus: every scenario parses and has exactly one fixture,
//! every fixture a scenario and records just what its shows ask, and each
//! fixtures file is in the one format the reference harness writes; and
//! every scenario, replayed on the kernel's own SWIs, leaves what its fixture
//! records.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tairix_riscos_kernel::corpus::{
    check_observation, format_observations, parse_observations, parse_scenarios, At, Flag, Item,
    Kind, Scenario, Step, Value, UNWRITTEN,
};
use tairix_riscos_kernel::memory::layout::{APPLICATION, RISC_OS_5};
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::os::{Config, Fail, Host, Kernel, Os, Pulse, Regs};

/// The corpus's groups: each scenarios file and its fixtures.
fn groups() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the corpus")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "scenarios"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn every_scenario_has_its_fixture_and_every_fixture_its_scenario() {
    let mut checked = 0;
    for path in groups() {
        checked += 1;
        let scenarios = parse_scenarios(&std::fs::read_to_string(&path).expect("scenarios"))
            .unwrap_or_else(|e| panic!("{}:{}: {}", path.display(), e.line, e.reason));
        let fixtures_path = path.with_extension("fixtures");
        let text = std::fs::read_to_string(&fixtures_path).expect("its fixtures");
        let fixtures = parse_observations(&text)
            .unwrap_or_else(|e| panic!("{}:{}: {}", fixtures_path.display(), e.line, e.reason));
        let names: Vec<&str> = scenarios.iter().map(|s| s.name.as_str()).collect();
        let fixed: Vec<&str> = fixtures.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            fixed,
            "{}: one fixture per scenario, in order",
            path.display()
        );
        for (scenario, fixture) in scenarios.iter().zip(&fixtures) {
            check_observation(scenario, fixture)
                .unwrap_or_else(|e| panic!("{}: its line {}: {}", scenario.name, e.line, e.reason));
        }
        let header: Vec<&str> = text
            .lines()
            .take_while(|line| line.starts_with("# "))
            .map(|line| &line[2..])
            .collect();
        assert_eq!(
            format_observations(&header, &fixtures),
            text,
            "{}: as the harness writes it",
            fixtures_path.display()
        );
    }
    assert!(checked > 0, "a corpus");
}

/// A host still at the session's start, with no input.
struct StillHost;

impl Host for StillHost {
    fn monotonic(&mut self) -> u64 {
        0
    }

    fn real_time(&mut self) -> u64 {
        0
    }

    fn pulse(&mut self) -> Pulse {
        Pulse::Continue
    }

    fn input(&mut self) -> Option<tairix_riscos_kernel::input::InputEvent> {
        None
    }

    fn wait(&mut self, _: Option<u64>) -> Pulse {
        Pulse::Stop
    }
}

/// The application space a replay works in: room for every string and
/// block a scenario passes.
const SPACE: u32 = 0x10_0000;

/// The zone the reference machine's UK territory keeps.
fn uk_time() -> tairix_tz::Zone<'static> {
    static STORE: OnceLock<Vec<u8>> = OnceLock::new();
    static INFO: OnceLock<tairix_tz::ZoneInfo<'static>> = OnceLock::new();
    let bytes = STORE.get_or_init(|| tairix_tzcompile::compile().expect("the store compiles"));
    let info = INFO.get_or_init(|| tairix_tz::ZoneInfo::parse(bytes).expect("the store"));
    info.zone("Europe/London").expect("in the store")
}

/// A kernel booted afresh, with application space for the strings and
/// blocks a scenario passes.
fn kernel() -> Kernel {
    let config = Config {
        layout: &RISC_OS_5,
        ceiling: Ceiling::derive(256 * 1024 * 1024),
        memo_pages: 16,
        host: Box::new(StillHost),
        vdu: None,
        modules: &[],
        zone: uk_time(),
    };
    let mut kernel = Kernel::new(config).expect("a kernel");
    let memory = kernel.memory();
    let slot = memory.create_slot(Bits::Bit32).expect("a slot");
    memory.set_slot_size(slot, SPACE).expect("its memory");
    memory.switch_slot(slot).expect("made current");
    kernel
}

/// A replay's place in application space: the SWI names it looks up at the
/// start, and the strings and blocks it passes after them.
struct Space {
    next: u32,
}

impl Space {
    const NAMES: u32 = APPLICATION;

    fn new() -> Self {
        Self {
            next: APPLICATION + 0x100,
        }
    }

    /// `bytes` of room, word-aligned as BASIC's are.
    fn take(&mut self, bytes: u32) -> u32 {
        let at = self.next;
        self.next += bytes.next_multiple_of(4);
        assert!(self.next <= APPLICATION + SPACE, "a replay's room");
        at
    }
}

fn kept_value(kept: &[(String, u32)], name: &str, offset: i32) -> u32 {
    kept.iter()
        .find(|(k, _)| k == name)
        .map(|&(_, v)| v.wrapping_add_signed(offset))
        .expect("kept before use")
}

/// Where a shown item is.
fn address(at: &At, regs: &Regs, kept: &[(String, u32)]) -> u32 {
    match at {
        At::Register(register) => regs.r[usize::from(*register)],
        At::Kept { name, offset } => kept_value(kept, name, *offset),
    }
}

/// What `item` of a show records, its key first; nothing for an item an
/// error leaves undefined.
fn shown(
    os: &mut Os<'_>,
    item: &Item,
    regs: &Regs,
    failed: bool,
    kept: &[(String, u32)],
) -> String {
    if failed && item.undefined_after_error() {
        return String::new();
    }
    let key = item.key();
    match item {
        Item::Register(register) => format!(" {key}=&{:08X}", regs.r[usize::from(*register)]),
        Item::Relative { register, from } => {
            let difference = regs.r[usize::from(*register)].wrapping_sub(kept_value(kept, from, 0));
            format!(" {key}=&{difference:08X}")
        }
        Item::Flag(flag) => {
            let set = match flag {
                Flag::V => failed,
                Flag::N => regs.flag(1 << 31),
                Flag::Z => regs.flag(1 << 30),
                Flag::C => regs.flag(1 << 29),
            };
            format!(" {key}={}", u8::from(set))
        }
        Item::Pointer { at, from } => {
            let word = os
                .read_word(address(at, regs, kept))
                .expect("the word shown");
            format!(
                " {key}=&{:08X}",
                word.wrapping_sub(kept_value(kept, from, 0))
            )
        }
        Item::Bytes { at, length } => {
            let address = address(at, regs, kept);
            let mut bytes = vec![0; *length as usize];
            os.read_bytes(address, &mut bytes).expect("the bytes shown");
            let mut text = format!(" {key}=");
            for byte in bytes {
                let _ = write!(text, "{byte:02X}");
            }
            text
        }
    }
}

/// What `scenario` leaves on the kernel, as the harness records it.
fn replay(scenario: &Scenario) -> Vec<String> {
    let mut kernel = kernel();
    let mut kept: Vec<(String, u32)> = Vec::new();
    let mut space = Space::new();
    let mut last = (Regs::default(), false);
    let mut calls = 0;
    let mut lines = Vec::new();
    for step in &scenario.steps {
        let mut os = kernel.os();
        match step {
            Step::Call { swi, registers } => {
                calls += 1;
                let mut name = swi.clone().into_bytes();
                name.push(0);
                os.write_bytes(Space::NAMES, &name)
                    .expect("the name written");
                let mut lookup = Regs::new([0, Space::NAMES, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                os.perform(0x2_0039, &mut lookup)
                    .unwrap_or_else(|_| panic!("{}: {swi} is not a SWI", scenario.name));
                let mut regs = Regs::default();
                for (register, value) in registers {
                    regs.r[usize::from(*register)] = match value {
                        Value::Number(number) => *number,
                        Value::Text(text) => {
                            let mut bytes = text.clone().into_bytes();
                            bytes.push(0);
                            let at = space.take(u32::try_from(bytes.len()).expect("short"));
                            os.write_bytes(at, &bytes).expect("the string written");
                            at
                        }
                        Value::Kept { name, offset } => kept_value(&kept, name, *offset),
                    };
                }
                let failed = match os.perform(lookup.r[0], &mut regs) {
                    Ok(()) => false,
                    Err(Fail::Error(block)) => {
                        regs.r[0] = block;
                        true
                    }
                    Err(Fail::Unwind(unwind)) => {
                        panic!("{}: {swi} unwound: {unwind:?}", scenario.name)
                    }
                };
                last = (regs, failed);
            }
            Step::Keep { register, name } => {
                let value = last.0.r[usize::from(*register)];
                kept.retain(|(k, _)| k != name);
                kept.push((name.clone(), value));
            }
            Step::Block { name, size } => {
                let at = space.take(*size);
                os.write_bytes(at, &vec![UNWRITTEN; *size as usize])
                    .expect("the block made");
                kept.retain(|(k, _)| k != name);
                kept.push((name.clone(), at));
            }
            Step::Write { at, bytes } => {
                let Value::Kept { name, offset } = at else {
                    panic!("a write is to a kept address");
                };
                os.write_bytes(kept_value(&kept, name, *offset), bytes)
                    .expect("the bytes written");
            }
            Step::Show { items } => {
                let (regs, failed) = last;
                let mut line = calls.to_string();
                for item in items {
                    line.push_str(&shown(&mut os, item, &regs, failed, &kept));
                }
                if failed {
                    let number = os.read_word(regs.r[0]).expect("an error block");
                    let _ = write!(line, " error=&{number:08X}");
                }
                lines.push(line);
            }
        }
    }
    lines
}

#[test]
fn every_scenario_replayed_on_the_kernel_leaves_what_its_fixture_records() {
    let mut differ = Vec::new();
    for path in groups() {
        let scenarios =
            parse_scenarios(&std::fs::read_to_string(&path).expect("scenarios")).expect("parsed");
        let text = std::fs::read_to_string(path.with_extension("fixtures")).expect("fixtures");
        let fixtures = parse_observations(&text).expect("parsed");
        let replayed = scenarios.iter().zip(&fixtures);
        for (scenario, fixture) in replayed.filter(|(s, _)| s.kind != Kind::Reference) {
            let ours = replay(scenario);
            for (index, (mine, theirs)) in ours.iter().zip(&fixture.lines).enumerate() {
                if mine != theirs {
                    differ.push(format!(
                        "{} {} line {}\n  RISC OS: {theirs}\n  ours:    {mine}",
                        path.file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into()),
                        scenario.name,
                        index + 1
                    ));
                }
            }
            if ours.len() != fixture.lines.len() {
                differ.push(format!("{}: a line per show", scenario.name));
            }
        }
    }
    assert!(
        differ.is_empty(),
        "{} lines differ from genuine RISC OS:\n{}",
        differ.len(),
        differ.join("\n")
    );
}
