//! The `wintersun` command line (`plans/APPS.md`).
//!
//! Closed: the game takes no operands, one option names the world to open,
//! and one asks for the reference scene in place of a world. Anything outside
//! the grammar is a usage error, never a guess.

use core::fmt::Write as _;

use tairix_abi::stdinfo::{Human, Severity, StdInfoKind, StdInfoRecord};
use tairix_abi::Errno;
use tairix_inline::ArrayString;

/// The usage banner a usage error is reported with, and what the short-help
/// switches print when the bundle's own Help tree cannot be read.
pub const USAGE: &str = "usage: wintersun [-h | -? | --help | --reference-scene | --seed SEED]";

/// The option asking for the reference scene.
pub const REFERENCE_SCENE: &str = "--reference-scene";

/// The option naming the seed of the world to open.
pub const SEED: &str = "--seed";

/// The switches asking for the command's own short help.
pub const HELP_SWITCHES: [&str; 3] = ["-h", "-?", "--help"];

/// The command's name, as its `stdinfo` records are produced under.
pub const PRODUCER: &str = "wintersun";

/// Room for the record [`drawn_seed_record`] writes, whatever the seed.
pub const SEED_RECORD_BYTES: usize = 512;

/// What a launch asks for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Launch {
    /// A world played: the one whose seed was named, or a newly drawn one.
    Play(Option<u64>),
    /// The reference scene (`crate::reference`), held still.
    ReferenceScene,
    /// The command's own short help.
    Help,
}

/// The one failure [`parse`] reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliError {
    /// The command line was not understood.
    Usage,
}

/// Parse `args`, the arguments after the program name.
///
/// Read left to right, as every command app reads its options: a short-help
/// switch wins where it is reached, a repeated option's last value stands,
/// and `--` ends the options. A seed is a decimal `u64`, given as `--seed N`
/// or `--seed=N`. The reference scene is one fixed realm, so naming a seed
/// for it is refused rather than ignored. The game takes no operands.
///
/// # Errors
///
/// [`CliError::Usage`] for an option outside the grammar reached before any
/// help switch, a seed that is not a decimal `u64`, a seed with the
/// reference scene, or an operand.
pub fn parse(args: &[&str]) -> Result<Launch, CliError> {
    let mut reference = false;
    let mut seed = None;
    let mut rest = args.iter();
    while let Some(&arg) = rest.next() {
        if HELP_SWITCHES.contains(&arg) {
            return Ok(Launch::Help);
        }
        match arg {
            REFERENCE_SCENE => reference = true,
            SEED => seed = Some(decimal(rest.next().copied())?),
            "--" => {
                if rest.next().is_some() {
                    return Err(CliError::Usage);
                }
                break;
            }
            _ => match arg.strip_prefix("--seed=") {
                Some(value) => seed = Some(decimal(Some(value))?),
                None => return Err(CliError::Usage),
            },
        }
    }
    match (reference, seed) {
        (true, Some(_)) => Err(CliError::Usage),
        (true, None) => Ok(Launch::ReferenceScene),
        (false, seed) => Ok(Launch::Play(seed)),
    }
}

/// A seed spelled in decimal digits and nothing else.
///
/// Digits only: `u64`'s own parser also takes a leading `+`, which the
/// Help's grammar does not.
fn decimal(value: Option<&str>) -> Result<u64, CliError> {
    let digits = value.ok_or(CliError::Usage)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CliError::Usage);
    }
    digits.parse().map_err(|_| CliError::Usage)
}

/// Write the `stdinfo` record a session that drew its own seed leaves, so the
/// same world can be opened again, into `out`; returns its length.
///
/// The seed travels as a string in the structured part as well as in the
/// command: a JSON number past 2⁵³ is not one every reader keeps exactly.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] when `out` cannot hold the line.
pub fn drawn_seed_record(seed: u64, out: &mut [u8]) -> Result<usize, Errno> {
    let mut message = ArrayString::<64>::new();
    let mut suggestion = ArrayString::<80>::new();
    let mut ai = ArrayString::<256>::new();
    write!(message, "Opened a new world, seed {seed}.").map_err(|_| Errno::BufferTooSmall)?;
    write!(
        suggestion,
        "Run `{PRODUCER} {SEED} {seed}` to open it again."
    )
    .map_err(|_| Errno::BufferTooSmall)?;
    write!(
        ai,
        "{{\"subject\":\"realm\",\"seed\":\"{seed}\",\"suggestion\":{{\"argv\":\
         [\"{PRODUCER}\",\"{SEED}\",\"{seed}\"],\"safe_to_autorun\":false,\
         \"requires_confirmation\":true}}}}"
    )
    .map_err(|_| Errno::BufferTooSmall)?;
    StdInfoRecord::new(
        PRODUCER,
        StdInfoKind::Context,
        "world.seed_drawn",
        Severity::Info,
        Human::with_suggestion(message.as_str(), suggestion.as_str()),
    )
    .with_ai(ai.as_str())
    .write_jsonl(out)
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
