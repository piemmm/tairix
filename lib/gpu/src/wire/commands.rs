//! Recorded commands on the wire, uploaded to a served device ahead of the
//! submission that runs them.
//!
//! What a client derived while recording — an overlap, a push constant's
//! words — crosses as what it was derived from, and is derived again where
//! it arrives: a served device trusts nothing a client concluded.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::{put_key, take_key, Reader, Wire, Writer};
use crate::command::{Command, DispatchRecord, Recorded};
use crate::model::ImageKey;
use crate::pass::{take_draws, DrawRecord, PassRecord};
use crate::transfer::{image_copy_overlaps, to_buffer_overlaps, to_image_overlaps};
use crate::{Error, ImageDesc, MAX_COPY_REGIONS, MAX_SUBMISSION_COMMANDS, MAX_UPDATE_BYTES};

/// What decoding commands asks of the device they arrive at.
pub(crate) trait Arrival {
    /// `bytes` of an update, charged to the context they arrive for.
    fn record_update(&self, bytes: Vec<u8>) -> Result<Arc<Recorded>, Error>;
    /// The description of the context's image `image`, where it has one.
    fn image_desc(&self, image: ImageKey) -> Option<ImageDesc>;
}

pub(crate) fn put_command(writer: &mut Writer<'_>, command: &Command) -> Result<(), Error> {
    match command {
        Command::Copy { src, dst, regions } => {
            0u8.put(writer)?;
            put_key(writer, *src)?;
            put_key(writer, *dst)?;
            writer.list(regions, |writer, region| region.put(writer))
        }
        &Command::Fill {
            dst,
            offset,
            size,
            word,
        } => {
            1u8.put(writer)?;
            put_key(writer, dst)?;
            offset.put(writer)?;
            size.put(writer)?;
            word.put(writer)
        }
        Command::Update { dst, offset, data } => {
            2u8.put(writer)?;
            put_key(writer, *dst)?;
            offset.put(writer)?;
            writer.blob(&data.bytes)
        }
        Command::BufferToImage {
            src, dst, regions, ..
        } => {
            3u8.put(writer)?;
            put_key(writer, *src)?;
            put_key(writer, *dst)?;
            writer.list(regions, |writer, region| region.put(writer))
        }
        Command::ImageToBuffer {
            src, dst, regions, ..
        } => {
            4u8.put(writer)?;
            put_key(writer, *src)?;
            put_key(writer, *dst)?;
            writer.list(regions, |writer, region| region.put(writer))
        }
        Command::CopyImage {
            src, dst, regions, ..
        } => {
            5u8.put(writer)?;
            put_key(writer, *src)?;
            put_key(writer, *dst)?;
            writer.list(regions, |writer, region| region.put(writer))
        }
        Command::ClearImage {
            image,
            value,
            ranges,
        } => {
            6u8.put(writer)?;
            put_key(writer, *image)?;
            value.put(writer)?;
            writer.list(ranges, |writer, range| range.put(writer))
        }
        Command::Dispatch(record) => {
            7u8.put(writer)?;
            record.put(writer)
        }
        Command::Pass(record) => {
            8u8.put(writer)?;
            record.put(writer)
        }
    }
}

/// A command as [`take_commands`] reads it, `Pass` with only its first
/// `draws` draws.
pub(crate) fn put_first_draws(
    writer: &mut Writer<'_>,
    pass: &PassRecord,
    draws: usize,
) -> Result<(), Error> {
    8u8.put(writer)?;
    pass.put_first(writer, draws)
}

/// Draws continuing the pass before them, as one command of a chunk.
pub(crate) fn put_more_draws(writer: &mut Writer<'_>, draws: &[DrawRecord]) -> Result<(), Error> {
    9u8.put(writer)?;
    writer.list(draws, |writer, draw| draw.put(writer))
}

/// Read commands into `into`, the commands they count as kept within
/// `room`; answers that count. Draws continuing a pass of an earlier upload
/// are left in `leading`, for the pass they continue.
pub(crate) fn take_commands(
    reader: &mut Reader<'_>,
    arrival: &dyn Arrival,
    (into, leading): (&mut Vec<Command>, &mut Vec<DrawRecord>),
    room: usize,
) -> Result<usize, Error> {
    let count = reader.count(MAX_SUBMISSION_COMMANDS)?;
    into.try_reserve(count).map_err(|_| Error::OutOfMemory)?;
    let mut counted = 0usize;
    for _ in 0..count {
        let left = room.saturating_sub(counted);
        let arrived = if u8::take(&mut reader.peek())? == 9 {
            u8::take(reader)?;
            let mut draws = take_draws(reader, left)?;
            let added = draws.len();
            match into.last_mut() {
                Some(Command::Pass(pass)) => pass.extend(&mut draws)?,
                // Only what opens a chunk continues an earlier upload's pass.
                None if leading.is_empty() => *leading = draws,
                _ => return Err(Error::Malformed),
            }
            added
        } else {
            let command = take_command(reader, arrival, left)?;
            let added = command.count();
            into.push(command);
            added
        };
        counted = counted
            .checked_add(arrived)
            .filter(|&counted| counted <= room)
            .ok_or(Error::TooMany)?;
    }
    Ok(counted)
}

fn take_command(
    reader: &mut Reader<'_>,
    arrival: &dyn Arrival,
    room: usize,
) -> Result<Command, Error> {
    Ok(match u8::take(reader)? {
        0 => Command::Copy {
            src: take_key(reader)?,
            dst: take_key(reader)?,
            regions: reader.list(MAX_COPY_REGIONS, Wire::take)?,
        },
        1 => Command::Fill {
            dst: take_key(reader)?,
            offset: Wire::take(reader)?,
            size: Wire::take(reader)?,
            word: Wire::take(reader)?,
        },
        2 => {
            let dst = take_key(reader)?;
            let offset = Wire::take(reader)?;
            let bytes = tairix_util::fallible::copied(reader.blob(MAX_UPDATE_BYTES)?)
                .ok_or(Error::OutOfMemory)?;
            Command::Update {
                dst,
                offset,
                data: arrival.record_update(bytes)?,
            }
        }
        3 => {
            let (src, dst) = (take_key(reader)?, take_key(reader)?);
            let regions = reader.list(MAX_COPY_REGIONS, Wire::take)?;
            let overlapping = to_image_overlaps(&regions)?;
            Command::BufferToImage {
                src,
                dst,
                regions,
                overlapping,
            }
        }
        4 => {
            let (src, dst) = (take_key(reader)?, take_key(reader)?);
            let regions = reader.list(MAX_COPY_REGIONS, Wire::take)?;
            // An image the context does not have is refused, stale, as the
            // submission resolves it.
            let overlapping = match arrival.image_desc(src) {
                Some(desc) => to_buffer_overlaps(&regions, &desc)?,
                None => false,
            };
            Command::ImageToBuffer {
                src,
                dst,
                regions,
                overlapping,
            }
        }
        5 => {
            let src: ImageKey = take_key(reader)?;
            let dst: ImageKey = take_key(reader)?;
            let regions = reader.list(MAX_COPY_REGIONS, Wire::take)?;
            let overlapping = image_copy_overlaps(&regions, src == dst)?;
            Command::CopyImage {
                src,
                dst,
                regions,
                overlapping,
            }
        }
        6 => Command::ClearImage {
            image: take_key(reader)?,
            value: Wire::take(reader)?,
            ranges: reader.list(MAX_COPY_REGIONS, Wire::take)?,
        },
        7 => Command::Dispatch(DispatchRecord::take(reader)?),
        8 => Command::Pass(
            Box::try_new(PassRecord::take(reader, room)?).map_err(|_| Error::OutOfMemory)?,
        ),
        _ => return Err(Error::Malformed),
    })
}
