//! System tasks: the closed set of privileged actions the desktop performs
//! for its own Settings application behind the system's authorisation prompt
//! ([`WindowRequest::Authorise`](super::WindowRequest::Authorise)).
//!
//! The task is all that crosses. It names no program, no account to run as,
//! no password and none of the prompt's words: the desktop maps each task to
//! what it runs and how, and says in its own words what is being allowed, so
//! no application decides what the system's prompt states or what it runs.

use crate::elevate::{ElevateWords, ELEVATE_MAX_OUTPUT, ELEVATE_WORDS_BYTES};
use crate::le::{put_i32, put_u16, put_u32, read_i32, read_u16, read_u32};
use crate::Errno;

/// One privileged action the desktop authorises and performs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SystemTask {
    /// Change the machine's configuration: `configure` with these words,
    /// alternating keys and values.
    Configure(ElevateWords),
    /// Change one account's fields: `usermod` with these words, which end
    /// with `--` and the account's name.
    ChangeAccount(ElevateWords),
    /// Replace one account's password: `passwd` with these words —
    /// `--record`, the derived record, `--` and the account's name.
    SetPassword(ElevateWords),
    /// Read the machine's configuration, its network addressing with it.
    ShowAddressing,
    /// Read every account and group.
    ShowAccounts,
    /// Open Date & Time, which sets the clock and the time zone.
    SetClock,
}

impl SystemTask {
    /// The words the task hands its program, or `None` for a task that
    /// carries none.
    #[must_use]
    pub const fn words(&self) -> Option<&ElevateWords> {
        match self {
            Self::Configure(words) | Self::ChangeAccount(words) | Self::SetPassword(words) => {
                Some(words)
            }
            Self::ShowAddressing | Self::ShowAccounts | Self::SetClock => None,
        }
    }

    /// Whether the wire carries this task: one that hands its program words
    /// hands it at least one, since with none each of those programs would
    /// read rather than change anything.
    pub(super) const fn encodable(&self) -> bool {
        match self.words() {
            Some(words) => !words.is_empty(),
            None => true,
        }
    }

    const fn kind(&self) -> u8 {
        match self {
            Self::Configure(_) => KIND_CONFIGURE,
            Self::ChangeAccount(_) => KIND_CHANGE_ACCOUNT,
            Self::SetPassword(_) => KIND_SET_PASSWORD,
            Self::ShowAddressing => KIND_SHOW_ADDRESSING,
            Self::ShowAccounts => KIND_SHOW_ACCOUNTS,
            Self::SetClock => KIND_SET_CLOCK,
        }
    }
}

/// Wire kind of [`SystemTask::Configure`].
const KIND_CONFIGURE: u8 = 1;
/// Wire kind of [`SystemTask::ChangeAccount`].
const KIND_CHANGE_ACCOUNT: u8 = 2;
/// Wire kind of [`SystemTask::SetPassword`].
const KIND_SET_PASSWORD: u8 = 3;
/// Wire kind of [`SystemTask::ShowAddressing`].
const KIND_SHOW_ADDRESSING: u8 = 4;
/// Wire kind of [`SystemTask::ShowAccounts`].
const KIND_SHOW_ACCOUNTS: u8 = 5;
/// Wire kind of [`SystemTask::SetClock`].
const KIND_SET_CLOCK: u8 = 6;

/// Byte offset of a task's kind within its operand block.
const KIND_AT: usize = 0;
/// Byte offset of a reserved byte, required zero.
const RESERVED_AT: usize = 1;
/// Byte offset of its word count.
const COUNT_AT: usize = 2;
/// Byte offset of its packed words' length.
const LEN_AT: usize = 4;
/// Byte offset of its packed words: each a `u16` length and its bytes.
const WORDS_AT: usize = 6;

/// Encoded size of `task`'s operand block: the fixed fields, then the words
/// it carries, so a task costs what it says.
pub(super) const fn wire_len(task: &SystemTask) -> usize {
    WORDS_AT
        + match task.words() {
            Some(words) => words.packed_len(),
            None => 0,
        }
}

/// The widest operand block any task encodes to.
pub(super) const MAX_WIRE_LEN: usize = WORDS_AT + ELEVATE_WORDS_BYTES;

/// Write `task`'s operand block at the front of `out`, which is exactly
/// [`wire_len`] bytes and already zeroed.
pub(super) fn write(out: &mut [u8], task: &SystemTask) {
    out[KIND_AT] = task.kind();
    if let Some(words) = task.words() {
        let (count, packed) = words.packed();
        put_u16(out, COUNT_AT, count);
        #[allow(clippy::cast_possible_truncation)] // Bounded by ELEVATE_WORDS_BYTES.
        put_u16(out, LEN_AT, packed.len() as u16);
        out[WORDS_AT..WORDS_AT + packed.len()].copy_from_slice(packed);
    }
}

/// Decode a task from `block`, which must be exactly the operand block its
/// own length describes: short is truncation, long is smuggling, and both
/// are refused, as is a task carrying words its kind does not take or
/// lacking words its kind does.
pub(super) fn read(block: &[u8]) -> Result<SystemTask, Errno> {
    let fixed = block.get(..WORDS_AT).ok_or(Errno::BufferTooSmall)?;
    if fixed[RESERVED_AT] != 0 {
        return Err(Errno::BadMagic);
    }
    let len = usize::from(read_u16(fixed, LEN_AT));
    match block.len().cmp(&(WORDS_AT + len)) {
        core::cmp::Ordering::Less => return Err(Errno::BufferTooSmall),
        core::cmp::Ordering::Greater => return Err(Errno::BadMagic),
        core::cmp::Ordering::Equal => {}
    }
    let words = ElevateWords::from_packed(read_u16(fixed, COUNT_AT), &block[WORDS_AT..])?;
    let task = match fixed[KIND_AT] {
        KIND_CONFIGURE => SystemTask::Configure(words),
        KIND_CHANGE_ACCOUNT => SystemTask::ChangeAccount(words),
        KIND_SET_PASSWORD => SystemTask::SetPassword(words),
        KIND_SHOW_ADDRESSING | KIND_SHOW_ACCOUNTS | KIND_SET_CLOCK if !words.is_empty() => {
            return Err(Errno::OutOfRange);
        }
        KIND_SHOW_ADDRESSING => SystemTask::ShowAddressing,
        KIND_SHOW_ACCOUNTS => SystemTask::ShowAccounts,
        KIND_SET_CLOCK => SystemTask::SetClock,
        _ => return Err(Errno::OutOfRange),
    };
    if !task.encodable() {
        return Err(Errno::OutOfRange);
    }
    Ok(task)
}

/// What became of one accepted [`SystemTask`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AuthorisedOutcome {
    /// It ran, and exited with this code.
    Ran {
        /// The program's exit status.
        exit_code: i32,
    },
    /// The application it opens was started, and is running.
    Launched,
    /// It ran, exited with this code, and printed `len` bytes, which the
    /// window takes with
    /// [`WindowRequest::TakeAuthorisedOutput`](super::WindowRequest::TakeAuthorisedOutput).
    Captured {
        /// The program's exit status.
        exit_code: i32,
        /// How much it printed, at most [`ELEVATE_MAX_OUTPUT`].
        len: u32,
    },
    /// It ran and exited with this code, but printed more than one answer
    /// carries, so none of it is offered.
    Overran {
        /// The program's exit status.
        exit_code: i32,
    },
    /// Nothing ran: the prompt was cancelled, or taken down unanswered.
    Declined,
}

/// Wire discriminant of [`AuthorisedOutcome::Ran`].
const OUTCOME_RAN: u8 = 1;
/// Wire discriminant of [`AuthorisedOutcome::Launched`].
const OUTCOME_LAUNCHED: u8 = 2;
/// Wire discriminant of [`AuthorisedOutcome::Captured`].
const OUTCOME_CAPTURED: u8 = 3;
/// Wire discriminant of [`AuthorisedOutcome::Overran`].
const OUTCOME_OVERRAN: u8 = 4;
/// Wire discriminant of [`AuthorisedOutcome::Declined`].
const OUTCOME_DECLINED: u8 = 5;

/// Byte offset, within an `Authorised` event frame, of the outcome.
const AUTHORISED_OUTCOME_OFFSET: usize = 16;
/// Byte offset of the exit code, zero for an outcome that ran nothing.
const AUTHORISED_EXIT_OFFSET: usize = 20;
/// Byte offset of the captured length, zero unless the outcome is
/// `Captured`.
const AUTHORISED_LEN_OFFSET: usize = 24;
/// End of an `Authorised`'s own block; the rest of the frame is reserved.
pub(super) const AUTHORISED_WIRE_END: usize = AUTHORISED_LEN_OFFSET + 4;

/// Write `outcome` into an event frame.
pub(super) fn write_outcome(out: &mut [u8], outcome: AuthorisedOutcome) {
    let (kind, exit_code, len) = match outcome {
        AuthorisedOutcome::Ran { exit_code } => (OUTCOME_RAN, exit_code, 0),
        AuthorisedOutcome::Launched => (OUTCOME_LAUNCHED, 0, 0),
        AuthorisedOutcome::Captured { exit_code, len } => (OUTCOME_CAPTURED, exit_code, len),
        AuthorisedOutcome::Overran { exit_code } => (OUTCOME_OVERRAN, exit_code, 0),
        AuthorisedOutcome::Declined => (OUTCOME_DECLINED, 0, 0),
    };
    out[AUTHORISED_OUTCOME_OFFSET] = kind;
    put_i32(out, AUTHORISED_EXIT_OFFSET, exit_code);
    put_u32(out, AUTHORISED_LEN_OFFSET, len);
}

/// Read an event frame's outcome. Every field the outcome does not carry is
/// zero, and a capture is no longer than one answer holds.
pub(super) fn read_outcome(bytes: &[u8]) -> Result<AuthorisedOutcome, Errno> {
    if bytes[AUTHORISED_OUTCOME_OFFSET + 1..AUTHORISED_EXIT_OFFSET]
        .iter()
        .any(|&b| b != 0)
    {
        return Err(Errno::BadMagic);
    }
    let exit_code = read_i32(bytes, AUTHORISED_EXIT_OFFSET);
    let len = read_u32(bytes, AUTHORISED_LEN_OFFSET);
    let ran_nothing = exit_code == 0 && len == 0;
    match bytes[AUTHORISED_OUTCOME_OFFSET] {
        OUTCOME_RAN if len == 0 => Ok(AuthorisedOutcome::Ran { exit_code }),
        OUTCOME_LAUNCHED if ran_nothing => Ok(AuthorisedOutcome::Launched),
        OUTCOME_CAPTURED if usize::try_from(len).is_ok_and(|len| len <= ELEVATE_MAX_OUTPUT) => {
            Ok(AuthorisedOutcome::Captured { exit_code, len })
        }
        OUTCOME_OVERRAN if len == 0 => Ok(AuthorisedOutcome::Overran { exit_code }),
        OUTCOME_DECLINED if ran_nothing => Ok(AuthorisedOutcome::Declined),
        _ => Err(Errno::OutOfRange),
    }
}

/// Longest reply to a
/// [`WindowRequest::TakeAuthorisedOutput`](super::WindowRequest::TakeAuthorisedOutput):
/// the status word, the output's length, and the most a captured task
/// prints.
pub const WINDOW_AUTHORISED_OUTPUT_REPLY_MAX: usize =
    OUTPUT_REPLY_BYTES_OFFSET + ELEVATE_MAX_OUTPUT;
/// Byte offset of the output's length.
const OUTPUT_REPLY_LEN_OFFSET: usize = crate::reply::STATUS_REPLY_LEN;
/// Byte offset of the output.
const OUTPUT_REPLY_BYTES_OFFSET: usize = OUTPUT_REPLY_LEN_OFFSET + 2;

/// Encode a `TakeAuthorisedOutput` outcome into `out`, answering the number
/// of bytes written: only as long as the output, and a refusal is the shared
/// status frame.
#[must_use]
pub fn encode_authorised_output_reply(
    out: &mut [u8; WINDOW_AUTHORISED_OUTPUT_REPLY_MAX],
    result: Result<&[u8], Errno>,
) -> usize {
    let output = match result {
        Ok(output) if output.len() <= ELEVATE_MAX_OUTPUT => output,
        Ok(_) => return refuse(out, Errno::LengthOutOfRange),
        Err(err) => return refuse(out, err),
    };
    out[..OUTPUT_REPLY_LEN_OFFSET].copy_from_slice(&crate::reply::encode_status_reply(Ok(())));
    #[allow(clippy::cast_possible_truncation)] // Bounded by ELEVATE_MAX_OUTPUT.
    put_u16(out, OUTPUT_REPLY_LEN_OFFSET, output.len() as u16);
    let end = OUTPUT_REPLY_BYTES_OFFSET + output.len();
    out[OUTPUT_REPLY_BYTES_OFFSET..end].copy_from_slice(output);
    end
}

/// Write the shared status frame refusing with `err`.
fn refuse(out: &mut [u8; WINDOW_AUTHORISED_OUTPUT_REPLY_MAX], err: Errno) -> usize {
    out[..OUTPUT_REPLY_LEN_OFFSET].copy_from_slice(&crate::reply::encode_status_reply(Err(err)));
    OUTPUT_REPLY_LEN_OFFSET
}

/// Decode a `TakeAuthorisedOutput` reply.
///
/// # Errors
///
/// * The refusal the session stated, for a status-frame reply.
/// * [`Errno::BufferTooSmall`] for a frame shorter than its own header.
/// * [`Errno::LengthOutOfRange`] for a frame whose length is not the one it
///   states, or a length past [`ELEVATE_MAX_OUTPUT`].
pub fn decode_authorised_output_reply(bytes: &[u8]) -> Result<&[u8], Errno> {
    if bytes.len() >= OUTPUT_REPLY_LEN_OFFSET {
        crate::reply::decode_status_reply(&bytes[..OUTPUT_REPLY_LEN_OFFSET])?;
    }
    if bytes.len() < OUTPUT_REPLY_BYTES_OFFSET {
        return Err(Errno::BufferTooSmall);
    }
    let len = usize::from(read_u16(bytes, OUTPUT_REPLY_LEN_OFFSET));
    let output = &bytes[OUTPUT_REPLY_BYTES_OFFSET..];
    if len > ELEVATE_MAX_OUTPUT || output.len() != len {
        return Err(Errno::LengthOutOfRange);
    }
    Ok(output)
}

#[cfg(test)]
#[path = "task_tests.rs"]
mod tests;
