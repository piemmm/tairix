//! Squash 0.31: `Squash_Compress` and `Squash_Decompress` over the caller's
//! workspace, a call at a time, the stream `lib/compress`'s Squash codes as
//! the module codes it. A call's input and output are carried through the
//! host a piece at a time, so a call of any size costs bounded host memory.

use alloc::vec::Vec;

use tairix_compress::squash::{
    self, compress, decompress, max_output, start_compression, start_decompression, Clearing,
    Progress, Status, COMPRESS_WORK, DECOMPRESS_WORK,
};

use crate::error::Error;
use crate::module::{Builtin, Swis, SWI_OUT_OF_RANGE};
use crate::os::{len32, Fail, Os, Regs};
use crate::swi::chunk::SQUASH as CHUNK;

/// A call's flags.
pub(crate) mod flags {
    /// The workspace's operation is carried on rather than a new one begun.
    pub(super) const CONTINUE: u32 = 1;
    /// More input follows this call's.
    pub(super) const MORE: u32 = 2;
    /// The caller says the output fits: a choice of algorithm, which changes
    /// nothing a call makes.
    pub(super) const FITS: u32 = 4;
    /// Only the workspace and output sizes are asked for.
    pub(crate) const QUERY: u32 = 8;
}

/// An output size the caller does not know or does not ask for.
const UNKNOWN: u32 = u32::MAX;

/// The most of a call's input, and of its output, the host holds at once.
const PIECE: usize = 64 * 1024;

const BAD_ADDRESS: Error = Error {
    number: 0x921,
    token: "SquashBadAddress",
};
const BAD_INPUT: Error = Error {
    number: 0x922,
    token: "SquashBadInput",
};
const BAD_WORKSPACE: Error = Error {
    number: 0x923,
    token: "SquashBadWorkspace",
};
const BAD_PARAMETERS: Error = Error {
    number: 0x924,
    token: "SquashBadParameters",
};

pub(crate) static SQUASH: Builtin = Builtin {
    title: "Squash",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: Some(Swis {
        chunk: CHUNK,
        prefix: "Squash",
        names: &["Compress", "Decompress"],
        handler: swi,
    }),
    services: None,
    commands: &[],
    init: None,
    finalise: None,
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[],
    code: None,
};

fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    let way = match offset {
        0 => Way::Compress,
        1 => Way::Decompress,
        _ => return Err(os.error_with(SWI_OUT_OF_RANGE, &[SQUASH.title.as_bytes()])),
    };
    let given = regs.r[0];
    if given & !(flags::CONTINUE | flags::MORE | flags::FITS | flags::QUERY) != 0 {
        return Err(os.error(BAD_PARAMETERS));
    }
    if given & flags::QUERY != 0 {
        if given != flags::QUERY {
            return Err(os.error(BAD_PARAMETERS));
        }
        regs.r[0] = len32(way.work());
        regs.r[1] = match way {
            Way::Compress if regs.r[1] != UNKNOWN => max_output(regs.r[1]),
            Way::Compress | Way::Decompress => UNKNOWN,
        };
        return Ok(());
    }
    run(os, regs, way)
}

/// Which SWI a call is.
#[derive(Clone, Copy)]
enum Way {
    Compress,
    Decompress,
}

impl Way {
    const fn work(self) -> usize {
        match self {
            Self::Compress => COMPRESS_WORK,
            Self::Decompress => DECOMPRESS_WORK,
        }
    }
}

/// A call's work: R1 the workspace, R2 and R3 the input and its length, R4
/// and R5 the output and its room, R2 to R5 moved on past what the call
/// took and made, R0 its status.
fn run(os: &mut Os<'_>, regs: &mut Regs, way: Way) -> Result<(), Fail> {
    let [given, work_at, mut input_at, mut input_left, mut output_at, mut room, ..] = regs.r;
    let more = given & flags::MORE != 0;
    // A workspace off a word boundary is refused; the module itself hangs.
    if !work_at.is_multiple_of(4) {
        return Err(os.error(BAD_WORKSPACE));
    }
    let input_end = u64::from(input_at) + u64::from(input_left);
    let output_end = u64::from(output_at) + u64::from(room);
    let work_end = u64::from(work_at) + u64::from(len32(way.work()));
    // The workspace is written back last, so one the call cannot write is
    // refused before any output is made.
    if !os.readable(input_at, input_end)
        || !os.writable(output_at, output_end)
        || !os.writable(work_at, work_end)
    {
        return Err(os.error(BAD_ADDRESS));
    }
    let mut work = os.read_vec(work_at, way.work())?;
    if given & flags::CONTINUE == 0 {
        let started = match way {
            Way::Compress => {
                start_compression(&mut work, Clearing::for_first_call(input_left, room, more))
            }
            Way::Decompress => start_decompression(&mut work),
        };
        started.map_err(|err| refused(os, err))?;
    }
    let mut input = Vec::new();
    let mut output =
        tairix_util::fallible::filled(piece(room), 0u8).ok_or_else(|| os.no_memory())?;
    let status = loop {
        let take = piece(input_left);
        let last = take == input_left as usize;
        input.clear();
        os.read_onto(&mut input, input_at, take)?;
        let out = &mut output[..piece(room)];
        let whole_room = out.len() == room as usize;
        let coder = match way {
            Way::Compress => compress,
            Way::Decompress => decompress,
        };
        let Progress {
            status,
            consumed,
            produced,
        } = coder(&mut work, &input, more || !last, out, &mut |_| {})
            .map_err(|err| refused(os, err))?;
        os.write_bytes(output_at, &out[..produced])?;
        input_at = input_at.wrapping_add(len32(consumed));
        input_left -= len32(consumed);
        output_at = output_at.wrapping_add(len32(produced));
        room -= len32(produced);
        match status {
            Status::Done => break 0,
            Status::Input if last => break 1,
            Status::Output if whole_room => break 2,
            Status::Input | Status::Output => os.yield_point()?,
        }
    };
    os.write_bytes(work_at, &work)?;
    regs.r[0] = status;
    regs.r[2] = input_at;
    regs.r[3] = input_left;
    regs.r[4] = output_at;
    regs.r[5] = room;
    Ok(())
}

/// The part of `left` bytes the host holds at once.
fn piece(left: u32) -> usize {
    usize::try_from(left).map_or(PIECE, |left| left.min(PIECE))
}

/// The module's error for the codec's refusal.
fn refused(os: &mut Os<'_>, err: squash::Error) -> Fail {
    os.error(match err {
        squash::Error::Input => BAD_INPUT,
        squash::Error::Workspace => BAD_WORKSPACE,
    })
}

#[cfg(test)]
mod tests;
