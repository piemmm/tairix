//! The disk's tracks formatted from the one reached, its share shown as it
//! grows, until a track fails or none is left; and a bad track's clusters
//! marked: all of a diskette's, a fixed disk's those whose sectors cannot
//! be read again.

use super::{
    at, bad_track, flag, has, ioctl, over_progress, percent, text, volume_of, PROGRESS_WIDTH,
};
use crate::dos::device::generic::{self, track};
use crate::dos::message::{decimal, decimal_in};
use crate::dos::program::{say, Flow, Id, Program, Routine};
use crate::dos::utility::HeldFat;
use crate::frames::{locals, Far, Fault, Outcome};
use tairix_fat::boot::Kind;
use tairix_fat::table;

/// The bytes one call formats at most: a fixed disk's tracks are verified
/// as many at a time as fit.
const CALL_BYTES: u32 = 0xFFFF;

locals! {
    /// The tracks from the one reached formatted: `0` once the last is,
    /// or the extended error of the track that failed, the format left at
    /// it. A fixed disk's tracks go several at a time until a call fails,
    /// then one at a time to find the one that did.
    pub struct FormatTracks {
        tracks: u16,
        singly: bool,
        error: u16,
    }
}

mod phase {
    pub const FORMATTED: u8 = 1;
    pub const SHOWN: u8 = 2;
    pub const SHOWN_BAD: u8 = 3;
    pub const COMPLETE: u8 = 4;
}

impl FormatTracks {
    /// The next call: as many tracks as one takes, or one.
    fn next(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let left = p.get32(at::LEFT);
        if left == 0 {
            *phase = phase::COMPLETE;
            return Flow::Call(say(
                p,
                &over_progress(text::COMPLETE, PROGRESS_WIDTH, b"\r\n"),
            ));
        }
        let per_call = volume_of(p).map_or(1, |bpb| {
            CALL_BYTES / (u32::from(bpb.sectors_per_track.max(1)) * 512)
        });
        let several = has(p, flag::FIXED) && !self.singly;
        self.tracks = if several {
            u16::try_from(per_call.min(left).max(1)).unwrap_or(1)
        } else {
            1
        };
        let special = if self.tracks > 1 { track::SEVERAL } else { 0 };
        let (head, cylinder) = (p.get16(at::HEAD), p.get16(at::CYLINDER));
        p.set8(at::PACKET + track::SPECIAL, special);
        p.set16(at::PACKET + track::HEAD, head);
        p.set16(at::PACKET + track::CYLINDER, cylinder);
        p.set16(at::PACKET + track::TRACKS, self.tracks);
        *phase = phase::FORMATTED;
        Flow::Call(ioctl(p, generic::FORMAT_TRACK, at::PACKET))
    }

    /// `tracks` more done: their share shown if it grew, then `then`.
    fn done(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, tracks: u16, then: u8) -> Flow {
        let done = p.get32(at::DONE) + u32::from(tracks);
        p.set32(at::DONE, done);
        let share = percent(done, p.get32(at::TRACK_COUNT));
        *phase = then;
        if share == p.get16(at::PERCENT) {
            return self.step(p, phase, Ok(0));
        }
        p.set16(at::PERCENT, share);
        let words = [&decimal_in(u32::from(share), 3)[..], text::PERCENT].concat();
        Flow::Call(say(p, &over_progress(&words, PROGRESS_WIDTH, b"\r")))
    }

    /// The format moved on past `tracks` tracks.
    fn advance(p: &mut Program<'_, '_>, tracks: u16) {
        let heads = volume_of(p).map_or(1, |bpb| bpb.heads.max(1));
        let (mut cylinder, mut head) = (p.get16(at::CYLINDER), p.get16(at::HEAD));
        for _ in 0..tracks {
            head += 1;
            if head >= heads {
                head = 0;
                cylinder = cylinder.saturating_add(1);
            }
        }
        p.set16(at::CYLINDER, cylinder);
        p.set16(at::HEAD, head);
        let left = p.get32(at::LEFT).saturating_sub(u32::from(tracks));
        p.set32(at::LEFT, left);
    }
}

impl Routine for FormatTracks {
    const ID: Id = Id::FormatTracks;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::FORMATTED => match child {
                Ok(_) => self.done(p, phase, self.tracks, phase::SHOWN),
                Err(Fault::Error(_)) if self.tracks > 1 => {
                    self.singly = true;
                    self.next(p, phase)
                }
                Err(Fault::Error(code)) if bad_track(code) => {
                    self.error = code;
                    self.done(p, phase, 1, phase::SHOWN_BAD)
                }
                Err(Fault::Error(code)) => Flow::Return(Ok(u32::from(code))),
                Err(fault) => Flow::Return(Err(fault)),
            },
            phase::SHOWN => match child {
                Ok(_) => {
                    Self::advance(p, self.tracks);
                    self.next(p, phase)
                }
                Err(fault) => Flow::Return(Err(fault)),
            },
            phase::SHOWN_BAD => match child {
                Ok(_) => Flow::Return(Ok(u32::from(self.error))),
                Err(fault) => Flow::Return(Err(fault)),
            },
            phase::COMPLETE => match child {
                Ok(_) => Flow::Return(Ok(0)),
                Err(fault) => Flow::Return(Err(fault)),
            },
            _ => self.next(p, phase),
        }
    }
}

locals! {
    /// The bad track whose first sector is `first`, of the volume, marked
    /// in the FAT: every cluster it holds of a diskette; of a fixed disk,
    /// the clusters whose sectors fail to be read again, a cluster at a
    /// time, each said as it is tried.
    pub struct FormatBad {
        first: u32,
        at: u16,
        count: u16,
    }
}

impl FormatBad {
    #[must_use]
    pub const fn of(first: u32) -> Self {
        Self {
            first,
            at: 0,
            count: 0,
        }
    }

    /// Marks the cluster holding the volume's sector `sector` bad, if it
    /// is one of its clusters.
    fn mark(p: &mut Program<'_, '_>, sector: u32) {
        let Some(bpb) = volume_of(p) else {
            return;
        };
        let start = p.get32(at::START);
        let Some(relative) = sector.checked_sub(start) else {
            return;
        };
        let cluster = relative / u32::from(bpb.sectors_per_cluster.max(1)) + 2;
        let max = u32::from(p.get16(at::MAX_CLUSTER));
        let (Ok(cluster), true) = (u16::try_from(cluster), cluster <= max) else {
            return;
        };
        let kind = if has(p, flag::BIG_FAT) {
            Kind::Fat16
        } else {
            Kind::Fat12
        };
        let fat = HeldFat {
            segment: p.get16(at::FAT),
            kind,
        };
        let bad = u16::try_from(table::bad(kind)).unwrap_or(u16::MAX);
        if fat.entry(p, cluster) != bad {
            fat.set_entry(p, cluster, bad);
        }
    }

    /// The next piece of a fixed disk's bad track read again: up to the
    /// next cluster's start, then a cluster at a time.
    fn try_next(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some(bpb) = volume_of(p) else {
            return Flow::Return(Ok(0));
        };
        let sectors = bpb.sectors_per_track.max(1);
        if self.at >= sectors {
            return Flow::Return(Ok(0));
        }
        let cluster = u32::from(bpb.sectors_per_cluster.max(1));
        let sector = self.first + u32::from(self.at);
        let into = sector.saturating_sub(p.get32(at::START)) % cluster;
        let buffered = p.get16(at::CLUSTER) != 0;
        let piece = if buffered {
            u16::try_from(cluster - into).unwrap_or(1)
        } else {
            1
        };
        self.count = piece.min(sectors - self.at);
        let unit = sector.saturating_sub(p.get32(at::START)) / cluster + 2;
        let words = [text::RECOVER, &decimal(unit)[..]].concat();
        *phase = recovering::SAID;
        Flow::Call(say(p, &over_progress(&words, 66, b"\r")))
    }
}

mod recovering {
    pub const SAID: u8 = 1;
    pub const READ: u8 = 2;
}

impl Routine for FormatBad {
    const ID: Id = Id::FormatBad;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            recovering::SAID => {
                if let Err(fault) = child {
                    return Flow::Return(Err(fault));
                }
                let buffer = match p.get16(at::CLUSTER) {
                    0 => Far::new(p.own, at::BUFFER),
                    segment => Far::new(segment, 0),
                };
                let (head, cylinder) = (p.get16(at::HEAD), p.get16(at::CYLINDER));
                p.set8(at::PACKET + track::SPECIAL, 0);
                p.set16(at::PACKET + track::HEAD, head);
                p.set16(at::PACKET + track::CYLINDER, cylinder);
                p.set16(at::PACKET + track::FIRST, self.at);
                p.set16(at::PACKET + track::COUNT, self.count);
                p.set_far(at::PACKET + track::BUFFER, buffer);
                *phase = recovering::READ;
                Flow::Call(ioctl(p, generic::READ_TRACK, at::PACKET))
            }
            recovering::READ => {
                match child {
                    Ok(_) => {}
                    Err(Fault::Error(_)) => Self::mark(p, self.first + u32::from(self.at)),
                    Err(fault) => return Flow::Return(Err(fault)),
                }
                self.at += self.count;
                self.try_next(p, phase)
            }
            _ if has(p, flag::FIXED) => self.try_next(p, phase),
            _ => {
                let sectors = volume_of(p).map_or(0, |bpb| bpb.sectors_per_track);
                for n in 0..u32::from(sectors) {
                    Self::mark(p, self.first + n);
                }
                Flow::Return(Ok(0))
            }
        }
    }
}
