//! A stream on a device: opened for its driver, its bytes each way — a byte
//! at a time through the stream's word, or through its buffer — its waits,
//! its I/O controls and its close.
//!
//! A wait gives the CPU up: the host waits for the clock or input, and the
//! interrupts its waking brings are taken, so a driver's ticker or event
//! routine moves the bytes. Escape ends a wait, acknowledged, as RISC OS 5's
//! DeviceFS acknowledges it. A write that waits offers `UpCall` 6 on the
//! stream's poll word each round, as RISC OS 5's does; a read only where the
//! stream's sleeping I/O is set.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::buffers::{flag as buffer_flag, reason as serve};
use tairix_riscos_kernel::filing::fs::file_info;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::os::{quietly, Entry, Fail, Os, Regs};
use tairix_riscos_kernel::service;
use tairix_riscos_kernel::swi::os::upcall::{
    BUFFER_EMPTYING, BUFFER_FILLING, SLEEP_NO_MORE, STREAM_CLOSED, STREAM_CREATED,
};
use tairix_riscos_kernel::swi::os::UPCALL;
use tairix_riscos_kernel::swi::{buffer, X};

use super::special;
use super::{fail, reason, with, Target, Way, BAD_OPERATION, NOT_FOUND, WAKE_TX};

/// A stream's block in the RMA, which is its handle, holds the poll word an
/// `UpCall` about it names where RISC OS 5's lies.
const POLL_WORD: u32 = 0x34;
const STREAM_BYTES: u32 = POLL_WORD + 4;
/// What the poll word holds: non-zero, so a task switcher hands control
/// back after each poll.
const POLLING: u32 = 2;

/// A stream's flags, as its driver's `Finalise` is given them: always set,
/// buffered, and transmitting.
const STREAM: u32 = 1 << 31;
const BUFFERED: u32 = 1 << 30;
const TRANSMITTING: u32 = 1;

/// The handle a stream with no buffer gives in its place, and a threshold
/// none is set for.
const NONE: u32 = u32::MAX;

/// The driver entries that monitor a waiting stream, and the I/O control
/// that passes to its driver.
const MONITOR_TX: u32 = 12;
const MONITOR_RX: u32 = 13;
const IOCTL: u32 = 14;

/// The centiseconds a monitored stream waits between its driver's calls:
/// the driver's chance to end a transfer its device has failed.
const MONITOR_PERIOD: u64 = 100;

/// An I/O control's group of DeviceFS's own, its reasons, and the access
/// bits of its third byte.
const OWN_GROUP: u8 = 0xFF;
const NON_BLOCKING: u16 = 1;
const SLEEPING: u16 = 4;
const SLEEP_TIMEOUT: u16 = 5;
const READ_ACCESS: u8 = 1 << 6;
const WRITE_ACCESS: u8 = 1 << 7;

/// How a stream's transfers wait, as its I/O controls set them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Options {
    /// A transfer that cannot finish gives what it has at once.
    non_blocking: bool,
    /// A read that waits offers `UpCall` 6, as a write always does.
    sleeping: bool,
    /// The centiseconds a sleeping transfer waits; nought for ever.
    timeout: u32,
}

/// A stream's buffer: its handle, and the buffer manager's service routine
/// and internal ID it is reached by; and whether the stream made it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Held {
    handle: u32,
    id: u32,
    routine: u32,
    workspace: u32,
    created: bool,
}

impl Held {
    /// The service routine for `reason` with R2 and R3: R2, R3 and C as it
    /// answers.
    fn serve(
        self,
        os: &mut Os<'_>,
        reason: u32,
        r2: u32,
        r3: u32,
    ) -> Result<(u32, u32, bool), Fail> {
        let mut regs = Regs::default();
        regs.r[..4].copy_from_slice(&[reason, self.id, r2, r3]);
        regs.r[12] = self.workspace;
        os.call_routine(self.routine, Bits::Bit32, Entry::Supervisor, &mut regs)?;
        Ok((regs.r[2], regs.r[3], regs.flag(psr::C)))
    }

    fn insert_byte(self, os: &mut Os<'_>, byte: u8) -> Result<bool, Fail> {
        let (_, _, full) = self.serve(os, serve::INSERT_BYTE, u32::from(byte), 0)?;
        Ok(!full)
    }

    fn remove_byte(self, os: &mut Os<'_>) -> Result<Option<u8>, Fail> {
        let (byte, _, empty) = self.serve(os, serve::REMOVE_BYTE, 0, 0)?;
        Ok((!empty).then_some(byte.to_le_bytes()[0]))
    }

    /// Up to `count` bytes in from `from`: how many went.
    fn insert_block(self, os: &mut Os<'_>, from: u32, count: u32) -> Result<u32, Fail> {
        let (_, left, _) = self.serve(os, serve::INSERT_BLOCK, from, count)?;
        Ok(count - left.min(count))
    }

    /// Up to `count` bytes out to `to`: how many came.
    fn remove_block(self, os: &mut Os<'_>, to: u32, count: u32) -> Result<u32, Fail> {
        let (_, left, _) = self.serve(os, serve::REMOVE_BLOCK, to, count)?;
        Ok(count - left.min(count))
    }

    fn used(self, os: &mut Os<'_>) -> Result<u32, Fail> {
        Ok(self.serve(os, serve::USED, 0, 0)?.0)
    }

    fn purge(self, os: &mut Os<'_>) -> Result<(), Fail> {
        self.serve(os, serve::PURGE, 0, 0).map(drop)
    }
}

/// An open stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Stream {
    /// DeviceFS's handle: its block in the RMA.
    pub(crate) handle: u32,
    /// FileSwitch's.
    pub(crate) file: u32,
    /// Its device's number.
    pub(crate) device: u32,
    target: Target,
    /// The driver's own handle for it.
    theirs: u32,
    way: Way,
    buffer: Option<Held>,
    /// The byte waiting to be read, or to be sent, of an unbuffered stream.
    word: Option<u8>,
    /// Its special field's block, or none.
    special: u32,
    flags: u32,
    options: Options,
    /// Its driver monitors it while it waits.
    monitored: bool,
}

/// The stream `handle` names; "device not found" for one closed under it.
fn fetch(os: &mut Os<'_>, handle: u32) -> Result<Stream, Fail> {
    with(os, |state| state.stream(handle).copied())?.ok_or_else(|| fail(os, NOT_FOUND))
}

/// The driver's entry for `reason` on `stream`, R2 its own handle and R3 to
/// R5 as given, its errors nobody's, as a transfer's courtesy calls are.
fn tell(
    os: &mut Os<'_>,
    stream: &Stream,
    reason: u32,
    r3: u32,
    r4: u32,
    r5: u32,
) -> Result<Regs, Fail> {
    tell_entered(os, stream, reason, [r3, r4, r5], Entry::Supervisor)
}

/// As [`tell`], entered as `entry` says.
fn tell_entered(
    os: &mut Os<'_>,
    stream: &Stream,
    reason: u32,
    values: [u32; 3],
    entry: Entry,
) -> Result<Regs, Fail> {
    let mut regs = Regs::default();
    quietly(ask(os, stream, reason, values, entry, &mut regs))?;
    Ok(regs)
}

/// The driver's entry `reason` for `stream`, R2 its handle for it, R3 to R5
/// `values` and R6 [`NONE`], with `regs`, entered as `entry` says: the error
/// it answers with, if it does.
fn ask(
    os: &mut Os<'_>,
    stream: &Stream,
    reason: u32,
    [r3, r4, r5]: [u32; 3],
    entry: Entry,
    regs: &mut Regs,
) -> Result<(), Fail> {
    regs.r[2..6].copy_from_slice(&[stream.theirs, r3, r4, r5]);
    regs.r[6] = NONE;
    stream.target.call(os, reason, regs, entry)
}

/// The handle of `stream`'s buffer, or [`NONE`].
fn buffer_handle(stream: &Stream) -> u32 {
    stream.buffer.map_or(NONE, |held| held.handle)
}

/// `UpCall` `number` for `stream`: R1 to R5 its driver, way, file, the
/// driver's handle and its buffer's, R6 `r6`, R7 the open's mode, and R8
/// DeviceFS's own handle, as RISC OS 5 makes them.
fn upcall(
    os: &mut Os<'_>,
    number: u32,
    stream: &Stream,
    r5: u32,
    r6: u32,
    mode: u32,
) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[..8].copy_from_slice(&[
        number,
        stream.target.handle,
        stream.way.upcall_word(),
        stream.file,
        stream.theirs,
        r5,
        r6,
        mode,
    ]);
    regs.r[8] = stream.handle;
    quietly(os.perform(X | UPCALL, &mut regs)).map_err(Fail::from)
}

/// What a transfer that cannot go on now may do.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Waiting {
    /// Give what it has where the stream's I/O controls say so.
    MayStop,
    /// Wait until it can: a byte written has no way to say it was not.
    Always,
}

/// Waits until `ready` answers: Escape, acknowledged, ends it with its
/// error, and the driver's monitoring with its. `UpCall` 6 first each round
/// where `asleep`, the host waiting unless a task switcher took it — until
/// input or timed work comes, or the next monitoring or the timeout falls
/// due. A non-blocking stream, or a sleeping one past its timeout, gives
/// none where `waiting` lets it.
fn block<T>(
    os: &mut Os<'_>,
    handle: u32,
    asleep: bool,
    monitor: u32,
    waiting: Waiting,
    mut ready: impl FnMut(&mut Os<'_>) -> Result<Option<T>, Fail>,
) -> Result<Option<T>, Fail> {
    if let Some(done) = ready(os)? {
        return Ok(Some(done));
    }
    let started = os.now();
    loop {
        let stream = fetch(os, handle)?;
        let options = stream.options;
        let timeout = (waiting == Waiting::MayStop && options.sleeping && options.timeout != 0)
            .then(|| started.saturating_add(u64::from(options.timeout)));
        if waiting == Waiting::MayStop
            && (options.non_blocking || timeout.is_some_and(|at| os.now() >= at))
        {
            return Ok(None);
        }
        if os.escape_set() {
            return Err(os.escape_acknowledged());
        }
        let taken = asleep && os.sleep_on(handle + POLL_WORD)?;
        if !taken {
            let monitoring = stream
                .monitored
                .then(|| os.now().saturating_add(MONITOR_PERIOD));
            match [timeout, monitoring].into_iter().flatten().min() {
                Some(deadline) => os.await_input_until(deadline)?,
                None => os.await_input()?,
            }
        }
        if stream.monitored {
            let mut regs = Regs::default();
            regs.r[2] = stream.theirs;
            stream
                .target
                .call(os, monitor, &mut regs, Entry::Supervisor)?;
        }
        if let Some(done) = ready(os)? {
            return Ok(Some(done));
        }
    }
}

/// The byte waiting in `handle`'s word, taken.
fn take_word(os: &mut Os<'_>, handle: u32) -> Result<Option<u8>, Fail> {
    with(os, |state| {
        state
            .stream_mut(handle)
            .and_then(|stream| stream.word.take())
    })
}

/// The byte a stream reads next: none where it may not wait for one.
pub(crate) fn get(os: &mut Os<'_>, handle: u32) -> Result<Option<u8>, Fail> {
    let stream = fetch(os, handle)?;
    if stream.way != Way::Receive {
        return Err(fail(os, BAD_OPERATION));
    }
    let asleep = stream.options.sleeping;
    match stream.buffer {
        None => {
            tell(os, &stream, reason::WAKE_RX, 1, 0, NONE)?;
            let got = take_word(os, handle)?;
            tell(os, &stream, reason::SLEEP_RX, 1, 0, NONE)?;
            if got.is_some() {
                return Ok(got);
            }
            block(os, handle, asleep, MONITOR_RX, Waiting::MayStop, |os| {
                take_word(os, handle)
            })
        }
        Some(held) => {
            tell(os, &stream, reason::WAKE_RX, 1, 0, held.handle)?;
            let got = held.remove_byte(os)?;
            let taken = u32::from(got.is_some());
            tell(os, &stream, reason::SLEEP_RX, 1 - taken, taken, held.handle)?;
            if got.is_some() {
                return Ok(got);
            }
            block(os, handle, asleep, MONITOR_RX, Waiting::MayStop, |os| {
                held.remove_byte(os)
            })
        }
    }
}

/// Up to `count` bytes of a buffered stream read to `to`, waiting for all
/// of them where it may: how many came.
pub(crate) fn read(os: &mut Os<'_>, handle: u32, to: u32, count: u32) -> Result<u32, Fail> {
    let stream = fetch(os, handle)?;
    let (Way::Receive, Some(held)) = (stream.way, stream.buffer) else {
        return Err(fail(os, BAD_OPERATION));
    };
    tell(os, &stream, reason::WAKE_RX, count, 0, held.handle)?;
    let mut got = held.remove_block(os, to, count)?;
    tell(os, &stream, reason::SLEEP_RX, count - got, got, held.handle)?;
    while got < count {
        let (at, left) = (to.wrapping_add(got), count - got);
        let came = block(
            os,
            handle,
            stream.options.sleeping,
            MONITOR_RX,
            Waiting::MayStop,
            |os| {
                held.remove_block(os, at, left)
                    .map(|n| (n > 0).then_some(n))
            },
        )?;
        match came {
            Some(n) => got += n,
            None => break,
        }
    }
    Ok(got)
}

/// `byte` written to a stream: into its word once the driver has taken the
/// last and the driver woken, or into its buffer, waiting for room.
pub(crate) fn put(os: &mut Os<'_>, handle: u32, byte: u8) -> Result<(), Fail> {
    let stream = fetch(os, handle)?;
    if stream.way != Way::Transmit {
        return Err(fail(os, BAD_OPERATION));
    }
    match stream.buffer {
        None => {
            let free = |os: &mut Os<'_>| {
                with(os, |state| {
                    state
                        .stream(handle)
                        .is_none_or(|s| s.word.is_none())
                        .then_some(())
                })
            };
            block(os, handle, true, MONITOR_TX, Waiting::Always, free)?;
            let stream = fetch(os, handle)?;
            with(os, |state| {
                if let Some(stream) = state.stream_mut(handle) {
                    stream.word = Some(byte);
                }
            })?;
            tell(os, &stream, reason::WAKE_TX, 0, 0, NONE).map(drop)
        }
        Some(held) => {
            if held.insert_byte(os, byte)? {
                return Ok(());
            }
            let fitted =
                |os: &mut Os<'_>| held.insert_byte(os, byte).map(|went| went.then_some(()));
            let waited = block(os, handle, true, MONITOR_TX, Waiting::Always, fitted);
            purging(os, held, waited).map(drop)
        }
    }
}

/// Up to `count` bytes of a buffered stream written from `from`, waiting for
/// room where it may: how many went.
pub(crate) fn write(os: &mut Os<'_>, handle: u32, from: u32, count: u32) -> Result<u32, Fail> {
    let stream = fetch(os, handle)?;
    let (Way::Transmit, Some(held)) = (stream.way, stream.buffer) else {
        return Err(fail(os, BAD_OPERATION));
    };
    let mut done = held.insert_block(os, from, count)?;
    while done < count {
        let (at, left) = (from.wrapping_add(done), count - done);
        let waited = block(os, handle, true, MONITOR_TX, Waiting::MayStop, |os| {
            held.insert_block(os, at, left)
                .map(|n| (n > 0).then_some(n))
        });
        let went = purging(os, held, waited)?;
        match went {
            Some(n) => done += n,
            None => break,
        }
    }
    Ok(done)
}

/// A transmit wait's outcome, the buffer emptied where it failed, as RISC OS
/// 5 empties it when Escape ends a write.
fn purging<T>(os: &mut Os<'_>, held: Held, outcome: Result<T, Fail>) -> Result<T, Fail> {
    if let Err(Fail::Error(_)) = outcome {
        held.purge(os)?;
    }
    outcome
}

/// The buffer manager's wake-up routine for a stream's transmit buffer, R8
/// the stream: its driver woken to send with interrupts off, as the buffer
/// manager calls its routine, and the buffer left dormant where the driver
/// stays so, for the next byte to wake it again.
pub(super) fn transmit_woken(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let Some(stream) = with(os, |state| state.stream(regs.r[8]).copied())? else {
        return Ok(());
    };
    let buffer = buffer_handle(&stream);
    let answer = tell_entered(
        os,
        &stream,
        reason::WAKE_TX,
        [0, 0, buffer],
        Entry::SupervisorNoIrq,
    )?;
    if answer.r[0] != 0 {
        return Ok(());
    }
    let mut dormant = Regs::default();
    dormant.r[..3].copy_from_slice(&[buffer_handle(&stream), 0, !buffer_flag::AWAKE]);
    quietly(os.perform(X | buffer::MODIFY_FLAGS, &mut dormant)).map_err(Fail::from)
}

/// DeviceFS's claim on `UpCallV`: a stream's buffer filling past its
/// threshold halts its driver, and emptying again resumes it. The call goes
/// on.
pub(super) fn upcalled(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let entry = match regs.r[0] {
        BUFFER_FILLING => reason::HALT,
        BUFFER_EMPTYING => reason::RESUME,
        _ => return Ok(()),
    };
    let buffer = regs.r[1];
    let found = with(os, |state| {
        state
            .streams
            .iter()
            .find(|stream| stream.buffer.is_some_and(|held| held.handle == buffer))
            .copied()
    })?;
    if let Some(stream) = found {
        tell(os, &stream, entry, 0, 0, buffer)?;
    }
    Ok(())
}

/// `DeviceFS_ReceivedCharacter`: R0's byte from R1's stream's driver, into
/// its word or its buffer; C set where it could not be taken. A handle that
/// names no receiving stream takes nothing.
pub(super) fn received(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (byte, handle) = (regs.byte(0), regs.r[1]);
    let held = with(os, |state| {
        let stream = state.stream_mut(handle).filter(|s| s.way == Way::Receive)?;
        match stream.buffer {
            None => {
                stream.word = Some(byte);
                Some(None)
            }
            Some(held) => Some(Some(held)),
        }
    })?;
    let taken = match held {
        None => false,
        Some(None) => true,
        Some(Some(held)) => held.insert_byte(os, byte)?,
    };
    regs.set_flag(psr::C, !taken);
    Ok(())
}

/// `DeviceFS_TransmitCharacter`: the next byte R1's stream sends, in R0;
/// C set where there is none.
pub(super) fn transmit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = regs.r[1];
    let held = with(os, |state| {
        let stream = state
            .stream_mut(handle)
            .filter(|s| s.way == Way::Transmit)?;
        Some(match stream.buffer {
            None => Err(stream.word.take()),
            Some(held) => Ok(held),
        })
    })?;
    let byte = match held {
        None => None,
        Some(Err(word)) => word,
        Some(Ok(held)) => held.remove_byte(os)?,
    };
    if let Some(byte) = byte {
        regs.r[0] = u32::from(byte);
    }
    regs.set_flag(psr::C, byte.is_none());
    Ok(())
}

/// `DeviceFS_Threshold`: R1's stream's buffer's threshold set to R2, or read
/// for -1; R2 the one it had. A stream with no buffer has none to set.
pub(super) fn threshold(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let held = with(os, |state| state.stream(regs.r[1]).and_then(|s| s.buffer))?;
    let Some(held) = held else {
        return Err(fail(os, BAD_OPERATION));
    };
    let mut set = Regs::default();
    set.r[..2].copy_from_slice(&[held.handle, regs.r[2]]);
    os.perform(X | buffer::THRESHOLD, &mut set)?;
    regs.r[2] = set.r[1];
    Ok(())
}

/// `OS_Args` 9 on a stream, the control's block at `block`: DeviceFS's own
/// group sets and reads how its transfers wait; any other goes to the
/// driver.
pub(crate) fn ioctl(os: &mut Os<'_>, handle: u32, block: u32) -> Result<(), Fail> {
    let stream = fetch(os, handle)?;
    let mut head = [0; 4];
    os.read_bytes(block, &mut head)?;
    let [low, high, group, access] = head;
    if group != OWN_GROUP {
        let mut regs = Regs::default();
        regs.r[..4].copy_from_slice(&[IOCTL, stream.file, stream.theirs, block]);
        regs.r[8] = stream.target.private;
        regs.r[12] = stream.target.workspace;
        return os.call_routine(
            stream.target.entry,
            stream.target.bits,
            Entry::Supervisor,
            &mut regs,
        );
    }
    let data = block.wrapping_add(4);
    let given = if access & WRITE_ACCESS != 0 {
        Some(os.read_word(data)?)
    } else {
        None
    };
    let reason = u16::from_le_bytes([low, high]);
    let now = with(os, |state| {
        let options = &mut state.stream_mut(handle)?.options;
        let value = match reason {
            NON_BLOCKING => {
                if let Some(value) = given {
                    options.non_blocking = value != 0;
                }
                u32::from(options.non_blocking)
            }
            SLEEPING => {
                if let Some(value) = given {
                    options.sleeping = value != 0;
                }
                u32::from(options.sleeping)
            }
            SLEEP_TIMEOUT => {
                if let Some(value) = given {
                    options.timeout = value;
                }
                options.timeout
            }
            _ => return None,
        };
        Some(value)
    })?;
    let Some(now) = now else {
        return Err(fail(os, BAD_OPERATION));
    };
    if access & READ_ACCESS != 0 {
        os.write_word(data, now)?;
    }
    Ok(())
}

/// The stream's extent as `OS_Args` 2 reads it: a sending stream's room, a
/// byte unbuffered and its buffer's size buffered; a receiving stream's
/// bytes held.
pub(crate) fn extent(os: &mut Os<'_>, handle: u32) -> Result<u32, Fail> {
    let stream = fetch(os, handle)?;
    match (stream.way, stream.buffer) {
        (Way::Transmit, None) => Ok(1),
        (Way::Receive, None) => Ok(u32::from(stream.word.is_some())),
        (Way::Transmit, Some(held)) => {
            let mut info = Regs::default();
            info.r[0] = held.handle;
            os.perform(X | buffer::GET_INFO, &mut info)?;
            Ok(info.r[2].wrapping_sub(info.r[1]))
        }
        (Way::Receive, Some(held)) => held.used(os),
    }
}

/// Whether a stream is at its end, as `OS_Args` 5 asks: not while its word
/// or buffer holds a byte, else as its driver's `EndOfData` answers.
pub(crate) fn at_end(os: &mut Os<'_>, handle: u32) -> Result<bool, Fail> {
    let stream = fetch(os, handle)?;
    let holding = match stream.buffer {
        None => stream.word.is_some(),
        Some(held) => held.used(os)? > 0,
    };
    if holding {
        return Ok(false);
    }
    let answer = tell(
        os,
        &stream,
        reason::END_OF_DATA,
        u32::MAX,
        0,
        buffer_handle(&stream),
    )?;
    Ok(answer.r[3] != 0)
}

/// What opening a stream needs of its device and driver.
#[derive(Clone, Debug)]
pub(crate) struct Opening {
    pub(crate) device: super::Device,
    pub(crate) target: Target,
    pub(crate) duplex: bool,
    pub(crate) most: [u32; 2],
    pub(crate) monitored: bool,
}

/// What an open made: DeviceFS's handle for the stream and the file
/// information word FileSwitch is answered with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Opened {
    pub(crate) handle: u32,
    pub(crate) info: u32,
}

/// A stream opened `way` on the device `opening` describes, for
/// FileSwitch's `file`, with the special field `special` and the device's
/// options, opened in `mode`: none where the device takes no stream that
/// way, or no more. A stream not made is told of by `UpCall` 11 as RISC OS
/// 5 tells of one: R2 nought whichever way, R4 no handle, R6 the streams
/// open that way already.
pub(crate) fn open(
    os: &mut Os<'_>,
    opening: &Opening,
    way: Way,
    file: u32,
    special: Option<&[u8]>,
    mode: u32,
) -> Result<Option<Opened>, Fail> {
    let refused = |os: &mut Os<'_>| -> Result<(), Fail> {
        let open = with(os, |state| {
            let on = state
                .streams
                .iter()
                .filter(|s| s.device == opening.device.id && s.way == way)
                .count();
            u32::try_from(on).unwrap_or(u32::MAX)
        })?;
        let mut regs = Regs::default();
        regs.r[..8].copy_from_slice(&[
            STREAM_CLOSED,
            opening.target.handle,
            0,
            file,
            0,
            0,
            open,
            mode,
        ]);
        quietly(os.perform(X | UPCALL, &mut regs)).map_err(Fail::from)
    };
    if !admitted(os, opening, way)? {
        refused(os)?;
        return Ok(None);
    }
    let variable = super::option_variable(os, &opening.device.name)?;
    let options = os.variable_text(&variable)?;
    let fields = [
        special.unwrap_or_default(),
        options.as_deref().unwrap_or_default(),
    ];
    let prepared =
        special_block(os, opening, &fields).and_then(|block| match suggested_buffer(os, &fields) {
            Ok(suggested) => Ok((block, suggested)),
            Err(fail) => {
                if block != 0 {
                    os.free_rma_quietly(block);
                }
                Err(fail)
            }
        });
    let not_made = |os: &mut Os<'_>, fail: Fail| match fail {
        Fail::Error(_) => refused(os).and(Err(fail)),
        Fail::Unwind(_) => Err(fail),
    };
    let (special_at, suggested) = match prepared {
        Ok(prepared) => prepared,
        Err(fail) => return not_made(os, fail),
    };
    match made(os, opening, way, file, special_at, suggested, mode) {
        Ok(opened) => Ok(Some(opened)),
        Err(Unmade::Before(fail)) => {
            if special_at != 0 {
                os.free_rma_quietly(special_at);
            }
            not_made(os, fail)
        }
        Err(Unmade::Closed(fail)) => Err(fail),
    }
}

/// Why a stream was not made: before it was kept, or once kept, closed
/// again as any stream is.
enum Unmade {
    Before(Fail),
    Closed(Fail),
}

/// Whether the device takes another stream `way`: within its driver's
/// limit, and on a device that is not full duplex, none open the other way.
/// Each stream in the way is offered to be closed by
/// `Service_DeviceFSCloseRequest` until it does.
fn admitted(os: &mut Os<'_>, opening: &Opening, way: Way) -> Result<bool, Fail> {
    let most = opening.most[way.index()];
    if most == 0 {
        return Ok(false);
    }
    loop {
        let blocking = with(os, |state| {
            let on = |stream: &&Stream| stream.device == opening.device.id;
            let same = state
                .streams
                .iter()
                .filter(on)
                .filter(|s| s.way == way)
                .count();
            let full = most != super::UNLIMITED && u32::try_from(same).unwrap_or(u32::MAX) >= most;
            state
                .streams
                .iter()
                .filter(on)
                .find(|stream| {
                    if stream.way == way {
                        full
                    } else {
                        !opening.duplex
                    }
                })
                .map(|stream| stream.file)
        })?;
        let Some(file) = blocking else {
            return Ok(true);
        };
        let mut request = Regs::default();
        request.r[..3].copy_from_slice(&[0, service::DEVICE_FS_CLOSE_REQUEST, file]);
        os.service(&mut request)?;
        let closed =
            request.r[1] == 0 && with(os, |state| state.streams.iter().all(|s| s.file != file))?;
        if !closed {
            return Ok(false);
        }
    }
}

/// The block the driver is given for the special field `fields` give: its
/// words where it registered a validation string, else the field as given;
/// none for no field.
fn special_block(os: &mut Os<'_>, opening: &Opening, fields: &[&[u8]; 2]) -> Result<u32, Fail> {
    let words = with(os, |state| {
        state
            .driver(opening.target.handle)
            .and_then(|driver| driver.validation.as_ref())
            .map(|validation| validation.words(fields))
    })?;
    let bytes = match words {
        Some(Ok(Some(words))) => {
            let bytes = words.iter().flat_map(|word| word.to_le_bytes());
            tairix_util::fallible::collected(words.len() * 4, bytes)
                .ok_or_else(|| os.no_memory())?
        }
        Some(Ok(None)) => return Err(os.no_memory()),
        Some(Err(error)) => return Err(os.error(tairix_riscos_kernel::error::obey_error(error))),
        None if fields[0].is_empty() => return Ok(0),
        None => {
            let mut text =
                tairix_util::fallible::copied(fields[0]).ok_or_else(|| os.no_memory())?;
            if !tairix_util::fallible::push(&mut text, 0) {
                return Err(os.no_memory());
            }
            text
        }
    };
    if bytes.is_empty() {
        return Ok(0);
    }
    os.rma_claim_with(&bytes)
}

/// The buffer DeviceFS's own keyword `buffer<n>` suggests, from `fields`.
fn suggested_buffer(os: &mut Os<'_>, fields: &[&[u8]; 2]) -> Result<u32, Fail> {
    let given = fields
        .iter()
        .flat_map(|field| field.split(|&b| b == b';'))
        .find_map(|item| special::given_as(b"buffer", item));
    match given {
        None => Ok(NONE),
        Some(rest) => match special::number(rest) {
            Ok((handle, _)) => Ok(handle),
            Err(error) => Err(os.error(tairix_riscos_kernel::error::obey_error(error))),
        },
    }
}

/// A stream's block from the RMA, cleared, its poll word polling.
fn stream_block(os: &mut Os<'_>) -> Result<u32, Fail> {
    let handle = os.rma_claim(STREAM_BYTES)?;
    if let Err(fail) = os
        .fill(handle, STREAM_BYTES, 0)
        .and_then(|()| os.write_word(handle + POLL_WORD, POLLING))
    {
        os.free_rma_quietly(handle);
        return Err(fail);
    }
    Ok(handle)
}

/// How many streams `device` has open `way`, the one being made counted.
fn streams_on(os: &mut Os<'_>, device: u32, way: Way) -> Result<u32, Fail> {
    with(os, |state| {
        let on = state
            .streams
            .iter()
            .filter(|s| s.device == device && s.way == way)
            .count();
        u32::try_from(on + 1).unwrap_or(u32::MAX)
    })
}

/// The stream made, past its admission: its block, the driver's
/// `Initialise`, its buffer, `StreamCreated` and `UpCall` 10.
fn made(
    os: &mut Os<'_>,
    opening: &Opening,
    way: Way,
    file: u32,
    special: u32,
    suggested: u32,
    mode: u32,
) -> Result<Opened, Unmade> {
    let handle = stream_block(os).map_err(Unmade::Before)?;
    let flags = STREAM
        | if way == Way::Transmit {
            TRANSMITTING
        } else {
            0
        };
    let mut stream = Stream {
        handle,
        file,
        device: opening.device.id,
        target: opening.target,
        theirs: 0,
        way,
        buffer: None,
        word: None,
        special,
        flags,
        options: Options::default(),
        monitored: opening.monitored,
    };
    let count = streams_on(os, stream.device, way).map_err(Unmade::Before)?;
    let mut initialise = Regs::default();
    initialise.r[2..8].copy_from_slice(&[handle, flags, file, count, special, mode]);
    if let Err(fail) =
        opening
            .target
            .call(os, reason::INITIALISE, &mut initialise, Entry::Supervisor)
    {
        os.free_rma_quietly(handle);
        return Err(Unmade::Before(fail));
    }
    stream.theirs = initialise.r[2];
    if let Err(fail) = keep(os, stream) {
        return Err(abandoned(os, &stream, fail));
    }
    let threshold = if opening.device.buffered {
        match buffered(os, &mut stream, opening, suggested, mode) {
            Ok(threshold) => threshold,
            Err(fail) => return Err(abandoned(os, &stream, fail)),
        }
    } else {
        0
    };
    let mut created = Regs::default();
    let buffer = buffer_handle(&stream);
    created.r[2..8].copy_from_slice(&[stream.theirs, buffer, file, buffer, threshold, mode]);
    if let Err(fail) =
        opening
            .target
            .call(os, reason::STREAM_CREATED, &mut created, Entry::Supervisor)
    {
        return Err(match shut(os, handle, Closer::FailedOpen) {
            Ok(()) => Unmade::Closed(fail),
            Err(closing) => Unmade::Closed(closing),
        });
    }
    if let Err(fail) = upcall(
        os,
        STREAM_CREATED,
        &stream,
        buffer_handle(&stream),
        threshold,
        mode,
    ) {
        discard(os, &stream, Closer::FailedOpen);
        return Err(Unmade::Closed(fail));
    }
    let access = match way {
        Way::Receive => file_info::READ,
        Way::Transmit => file_info::WRITE,
    };
    let direct = if stream.buffer.is_some() {
        file_info::GBPB
    } else {
        0
    };
    Ok(Opened {
        handle,
        info: access | direct,
    })
}

/// A stream given up before it was announced: its driver's `Finalise`
/// unless control is leaving, its buffer let go and its handle freed; the
/// block of its special field is the opener's to free.
fn abandoned(os: &mut Os<'_>, stream: &Stream, fail: Fail) -> Unmade {
    let fail = match fail {
        Fail::Error(_) => quietly(finalised(os, stream)).map_or_else(Fail::from, |()| fail),
        Fail::Unwind(_) => fail,
    };
    let fail = match (fail, detach(os, stream)) {
        (Fail::Error(_), Err(Fail::Unwind(unwind))) => Fail::Unwind(unwind),
        (fail, _) => fail,
    };
    os.free_rma_quietly(stream.handle);
    Unmade::Before(fail)
}

/// `stream` kept among the streams, in its handle's place.
fn keep(os: &mut Os<'_>, stream: Stream) -> Result<(), Fail> {
    let kept = with(os, |state| {
        let at = state.place(stream.handle).unwrap_or_else(|at| at);
        if state.streams.try_reserve(1).is_err() {
            return false;
        }
        state.streams.insert(at, stream);
        true
    })?;
    if kept {
        Ok(())
    } else {
        Err(os.no_memory())
    }
}

/// `stream`'s buffer: the driver's `CreateBuffer` given the device's
/// suggestion to change, then the buffer it names used, or one made, and
/// linked to DeviceFS; the threshold it set.
fn buffered(
    os: &mut Os<'_>,
    stream: &mut Stream,
    opening: &Opening,
    suggested: u32,
    mode: u32,
) -> Result<u32, Fail> {
    let (flags, size) = opening.device.buffers[stream.way.index()];
    let entry = match stream.way {
        Way::Transmit => reason::CREATE_TX_BUFFER,
        Way::Receive => reason::CREATE_RX_BUFFER,
    };
    let mut create = Regs::default();
    create.r[2..8].copy_from_slice(&[stream.theirs, flags, size, suggested, NONE, mode]);
    stream
        .target
        .call(os, entry, &mut create, Entry::Supervisor)?;
    let [_, _, _, flags, size, named, threshold, ..] = create.r;
    let mut probe = Regs::default();
    probe.r[0] = named;
    let exists = named != NONE && os.perform_quietly(X | buffer::GET_INFO, &mut probe)?;
    let handle = if exists {
        named
    } else {
        let mut make = Regs::default();
        make.r[..3].copy_from_slice(&[flags, size, named]);
        os.perform(X | buffer::CREATE, &mut make)?;
        make.r[0]
    };
    let linked = link(os, stream, handle);
    let held = match linked {
        Ok(held) => held,
        Err(fail) => {
            if !exists {
                let mut remove = Regs::default();
                remove.r[0] = handle;
                quietly(os.perform(X | buffer::REMOVE, &mut remove))?;
            }
            return Err(fail);
        }
    };
    let held = Held {
        created: !exists,
        ..held
    };
    stream.buffer = Some(held);
    stream.flags |= BUFFERED;
    with(os, |state| {
        if let Some(kept) = state.stream_mut(stream.handle) {
            *kept = *stream;
        }
    })?;
    if threshold != NONE {
        let mut set = Regs::default();
        set.r[..2].copy_from_slice(&[handle, threshold]);
        os.perform(X | buffer::THRESHOLD, &mut set)?;
    }
    Ok(threshold)
}

/// The buffer `handle` linked to `stream`, its transmit buffer woken through
/// DeviceFS; how it is reached.
fn link(os: &mut Os<'_>, stream: &Stream, handle: u32) -> Result<Held, Fail> {
    let wake = match stream.way {
        Way::Transmit => os.routine_of(&super::DEVICE_FS, WAKE_TX).unwrap_or(0),
        Way::Receive => 0,
    };
    let workspace = with(os, |state| state.workspace)?;
    let mut regs = Regs::default();
    regs.r[..5].copy_from_slice(&[handle, wake, 0, stream.handle, workspace]);
    os.perform(X | buffer::LINK_DEVICE, &mut regs)?;
    let mut internal = Regs::default();
    internal.r[0] = handle;
    if let Err(fail) = os.perform(X | buffer::INTERNAL_INFO, &mut internal) {
        unlinked(os, handle)?;
        return Err(fail);
    }
    Ok(Held {
        handle,
        id: internal.r[0],
        routine: internal.r[1],
        workspace: internal.r[2],
        created: false,
    })
}

/// The driver's `Finalise` for `stream`: the error it answers with, which
/// RISC OS 5 answers a close with, the stream left open.
fn finalised(os: &mut Os<'_>, stream: &Stream) -> Result<(), Fail> {
    let mut regs = Regs::default();
    ask(
        os,
        stream,
        reason::FINALISE,
        [0, 0, stream.flags],
        Entry::Supervisor,
        &mut regs,
    )
}

/// Who closes a stream: whether its driver may keep it open, and what
/// becomes of its handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Closer {
    /// Its holder. A driver refusing `Finalise` keeps it open, as FileSwitch
    /// then keeps the file; its handle is freed with it.
    Holder,
    /// Its device's removal. It closes whatever its driver answers, and its
    /// handle stays claimed until the holder closes it, so no stream made
    /// meanwhile is given the number the holder still has; the removal
    /// reserves the room to record it first.
    Removal,
    /// The open that made it, failing. It closes whatever its driver answers,
    /// and its handle, which no holder was given, is freed.
    FailedOpen,
}

/// A stream closed by `closer`: what it sends drained first — Escape ends
/// the wait, its buffer emptied — and `EndOfData`, then `UpCall` 7 on its
/// poll word, the driver's `Finalise`, its buffer let go, `UpCall` 11, and
/// its blocks freed. A handle that names no stream closes nothing, but for
/// one kept since its device's removal, which its holder's close frees. An
/// open failing as control leaves still lets its stream go, telling no one.
pub(crate) fn shut(os: &mut Os<'_>, handle: u32, closer: Closer) -> Result<(), Fail> {
    let Some(stream) = with(os, |state| state.stream(handle).copied())? else {
        if closer == Closer::Holder && with(os, |state| state.forget_kept(handle))? {
            os.free_rma_quietly(handle);
        }
        return Ok(());
    };
    match farewell(os, &stream) {
        Ok(false) => return Ok(()),
        Err(Fail::Error(error)) if closer == Closer::Holder => return Err(Fail::Error(error)),
        Ok(true) | Err(Fail::Error(_)) => {}
        Err(unwind) => {
            if closer == Closer::FailedOpen {
                discard(os, &stream, closer);
            }
            return Err(unwind);
        }
    }
    detach(os, &stream)?;
    upcall(
        os,
        STREAM_CLOSED,
        &stream,
        stream.flags,
        buffer_handle(&stream),
        0,
    )?;
    let_go(os, &stream, closer)
}

/// What a closing stream is told before it goes: what it sends drained —
/// Escape ends the wait, its buffer emptied — and `EndOfData`, `UpCall` 7 on
/// its poll word and its driver's `Finalise`; whether it is still open, as
/// a wait may find it closed meanwhile.
fn farewell(os: &mut Os<'_>, stream: &Stream) -> Result<bool, Fail> {
    let handle = stream.handle;
    if stream.way == Way::Transmit {
        let drained = |os: &mut Os<'_>| {
            let Some(now) = with(os, |state| state.stream(handle).copied())? else {
                return Ok(Some(()));
            };
            let holding = match now.buffer {
                None => now.word.is_some(),
                Some(held) => held.used(os)? > 0,
            };
            Ok((!holding).then_some(()))
        };
        let waited = block(os, handle, true, MONITOR_TX, Waiting::Always, drained);
        if with(os, |state| state.stream(handle).is_none())? {
            return Ok(false);
        }
        match waited {
            Ok(_) => {}
            Err(Fail::Error(_)) => {
                if let Some(held) = stream.buffer {
                    held.purge(os)?;
                }
            }
            Err(unwind) => return Err(unwind),
        }
        tell(
            os,
            stream,
            reason::END_OF_DATA,
            u32::MAX,
            0,
            buffer_handle(stream),
        )?;
    }
    let mut no_more = Regs::default();
    no_more.r[..2].copy_from_slice(&[SLEEP_NO_MORE, handle + POLL_WORD]);
    no_more.r[5..7].copy_from_slice(&[stream.flags, buffer_handle(stream)]);
    quietly(os.perform(X | UPCALL, &mut no_more))?;
    finalised(os, stream)?;
    Ok(true)
}

/// The stream's buffer let go — unlinked, and removed where the stream made
/// it — and the stream taken from the streams whatever that came to.
fn detach(os: &mut Os<'_>, stream: &Stream) -> Result<(), Fail> {
    let unlinked = stream.buffer.map_or(Ok(()), |held| unlink(os, held));
    with(os, |state| {
        state.streams.retain(|s| s.handle != stream.handle);
    })?;
    unlinked
}

/// The buffer `held` unlinked from its stream, and removed where the stream
/// made it.
fn unlink(os: &mut Os<'_>, held: Held) -> Result<(), Fail> {
    unlinked(os, held.handle)?;
    if held.created {
        let mut remove = Regs::default();
        remove.r[0] = held.handle;
        quietly(os.perform(X | buffer::REMOVE, &mut remove))?;
    }
    Ok(())
}

/// The buffer `handle` unlinked from the stream it serves.
fn unlinked(os: &mut Os<'_>, handle: u32) -> Result<(), Fail> {
    let mut unlink = Regs::default();
    unlink.r[0] = handle;
    quietly(os.perform(X | buffer::UNLINK_DEVICE, &mut unlink)).map_err(Fail::from)
}

/// A detached stream's blocks freed: its special field's, and its handle
/// unless its holder still has it, when the handle is kept claimed.
fn let_go(os: &mut Os<'_>, stream: &Stream, closer: Closer) -> Result<(), Fail> {
    if stream.special != 0 {
        os.free_rma_quietly(stream.special);
    }
    match closer {
        Closer::Removal => with(os, |state| state.keep(stream.handle)),
        Closer::Holder | Closer::FailedOpen => {
            os.free_rma_quietly(stream.handle);
            Ok(())
        }
    }
}

/// A stream let go with no one told, as control leaves: the unwind leaving
/// is what the caller sees, whatever letting the stream go comes to.
fn discard(os: &mut Os<'_>, stream: &Stream, closer: Closer) {
    let _ = detach(os, stream);
    let _ = let_go(os, stream, closer);
}
