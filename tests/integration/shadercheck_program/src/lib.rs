//! The `shadercheck` fixture's shared vocabulary (`plans/GPU.md` GP8, MG3):
//! the command word it installs under, the kernels its disk plants and what
//! `gpucheck --shader` must answer each, and the record the vertical's guest
//! gate waits on.
//!
//! The fixture, the guest kernel's sink and `tools/xtask` all read this
//! module, so the kernels planted, the answers expected and the record
//! judged cannot drift apart.

#![no_std]
#![deny(missing_docs)]

use tairix_log::{EventId, Level};

/// The command word the fixture bundle is installed under.
pub const COMMAND: &str = "shadercheck";

/// The record the fixture logs once every kernel was answered as it must be.
pub const PASSED: EventId = EventId(4600);

/// The record the fixture logs naming the first kernel answered otherwise.
pub const FAILED: EventId = EventId(4601);

/// [`PASSED`]'s message, which the vertical's script waits on.
pub const PASSED_MESSAGE: &str = "shadercheck: every kernel answered as it must";

/// [`FAILED`]'s message.
pub const FAILED_MESSAGE: &str = "shadercheck: a kernel was answered otherwise";

/// The level both records are logged at.
pub const LEVEL: Level = Level::Info;

/// The folder of the account's home the kernels are planted in.
pub const FOLDER: [&str; 2] = ["Home", "Shaders"];

/// What `gpucheck --shader` must answer a kernel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expect {
    /// Every served GPU agrees with the software device on the words it
    /// left.
    Agrees,
    /// Every served GPU agrees with the software device that the kernel ran
    /// past its deadline, and served on.
    Lost,
    /// The kernel is refused before any device is given it, with this text
    /// in the refusal.
    Refused(&'static str),
}

/// One kernel the disk plants and what it must be answered.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    /// The file it is planted as, in [`FOLDER`].
    pub file: &'static str,
    /// What it holds; `None` for [`HONEST_SPIRV`], which `tools/xtask`
    /// compiles from [`HONEST`] as it plants it.
    pub source: Option<&'static [u8]>,
    /// What `gpucheck --shader` must answer it.
    pub expect: Expect,
}

/// Each word made three times itself and one more.
pub const HONEST: &str = "
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if id.x < arrayLength(&data) {
        data[id.x] = data[id.x] * 3u + 1u;
    }
}
";

/// Reads and writes past private, workgroup and function arrays and past
/// its buffer: every read past what it reaches is zero, so each word ends as
/// twice its index, and every write past it lands nowhere.
pub const PAST: &str = "
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
var<private> held: array<u32, 4>;
var<workgroup> pool: array<u32, 4>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3u) {
    let n = arrayLength(&data);
    let i = id.x;
    if i < n {
        var local: array<u32, 4>;
        let far = data[i] + 4u;
        held[far] = 9u;
        local[far] = 7u;
        pool[far] = 5u;
        let past = held[far] + local[far] + pool[far] + data[n + i];
        data[n + i] = 0xdeadbeefu;
        data[i] = past * 1000u + held[i % 4u] + local[i % 4u] + 2u * i;
    }
}
";

/// Never ends: its buffer's first word is never all ones.
pub const ENDLESS: &str = "
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(1)
fn main() {
    loop {
        if data[0] == 0xffffffffu { break; }
        data[1] = data[1] + 1u;
    }
}
";

/// A WGSL kernel the front end refuses: a float stored as an integer.
pub const REFUSED_WGSL: &str = "
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(1)
fn main() {
    let half: u32 = 0.5;
    data[0] = half;
}
";

/// A SPIR-V module the validator refuses at its second capability, `Int8`,
/// which it does not admit: the header, `OpCapability Shader`,
/// `OpCapability Int8`, and `OpMemoryModel Logical GLSL450`, each word
/// little-endian.
pub const REFUSED_SPIRV: [u8; 48] = words([
    0x0723_0203,
    0x0001_0300,
    0,
    1,
    0,
    0x0002_0011,
    1,
    0x0002_0011,
    39,
    0x0003_000e,
    0,
    1,
]);

/// The text `gpucheck` names [`REFUSED_SPIRV`]'s capability by.
pub const REFUSED_CAPABILITY: &str = "capability 39, which is not admitted";

/// The file [`HONEST`] compiled to SPIR-V is planted as.
pub const HONEST_SPIRV: &str = "honest.spv";

/// Every kernel the disk plants, in the order the fixture runs them.
pub const CASES: [Case; 6] = [
    Case {
        file: "honest.wgsl",
        source: Some(HONEST.as_bytes()),
        expect: Expect::Agrees,
    },
    Case {
        file: HONEST_SPIRV,
        source: None,
        expect: Expect::Agrees,
    },
    Case {
        file: "past.wgsl",
        source: Some(PAST.as_bytes()),
        expect: Expect::Agrees,
    },
    Case {
        file: "endless.wgsl",
        source: Some(ENDLESS.as_bytes()),
        expect: Expect::Lost,
    },
    Case {
        file: "refused.wgsl",
        source: Some(REFUSED_WGSL.as_bytes()),
        expect: Expect::Refused(": error: "),
    },
    Case {
        file: "refused.spv",
        source: Some(&REFUSED_SPIRV),
        expect: Expect::Refused(REFUSED_CAPABILITY),
    },
];

/// What every served GPU's line holds where it agreed.
pub const AGREES: &str = "agrees with the software device";

/// What every served GPU's line holds where both lost the kernel past its
/// deadline and served on.
pub const LOST: &str = "each lost the kernel as it ran past its deadline, and served on";

/// `list`'s words, each little-endian.
const fn words<const W: usize, const B: usize>(list: [u32; W]) -> [u8; B] {
    let mut bytes = [0; B];
    let mut at = 0;
    while at < W && at * 4 + 3 < B {
        let word = list[at].to_le_bytes();
        bytes[at * 4] = word[0];
        bytes[at * 4 + 1] = word[1];
        bytes[at * 4 + 2] = word[2];
        bytes[at * 4 + 3] = word[3];
        at += 1;
    }
    bytes
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{Expect, CASES, REFUSED_SPIRV};

    #[test]
    fn the_refused_module_is_a_module_the_validator_refuses_by_its_capability() {
        let module = tairix_spirv::Module::decode(&REFUSED_SPIRV).expect("decoded");
        let refused = tairix_spirv::Admitted::new(module).expect_err("refused");
        assert_eq!(
            refused.rule,
            tairix_spirv::Rule::Capability(tairix_spirv::enumerant::Capability::INT8)
        );
        assert!(std::format!("{refused}").contains(super::REFUSED_CAPABILITY));
    }

    #[test]
    fn every_kernel_but_the_refused_one_compiles_and_every_file_is_named_once() {
        for case in CASES {
            let Some(source) = case.source else {
                continue;
            };
            let Ok(text) = core::str::from_utf8(source) else {
                continue;
            };
            let compiled = tairix_wgsl::compile(text, &tairix_wgsl::Options::default());
            assert_eq!(
                compiled.is_ok(),
                !matches!(case.expect, Expect::Refused(_)),
                "{}",
                case.file
            );
        }
        for (at, case) in CASES.iter().enumerate() {
            assert!(
                CASES
                    .iter()
                    .skip(at + 1)
                    .all(|other| other.file != case.file),
                "{}",
                case.file
            );
        }
    }
}
