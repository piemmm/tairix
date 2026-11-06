//! Seeking: the file's own index where it holds one, else the Clusters found
//! by walking it.

use alloc::vec::Vec;

use tairix_sound::SoundInput;
use tairix_util::fallible;

use super::super::id;
use super::super::MAX_INDEX_POINTS;
use super::{
    known, once, read_head, slurp, uint, Children, DemuxError, Opened, Step, Walk, Widths,
};

/// A point a seek lands on: a tick of the timeline, and the Cluster to read
/// on from to reach it.
#[derive(Clone, Copy, Debug)]
struct Point {
    tick: u64,
    cluster: u64,
}

/// Where the points come from.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// The file's `Cues` element at `at`: once its header is read, the next of
    /// its elements to read and where it ends.
    Cues { at: u64, read: Option<(u64, u64)> },
    /// The `Cues` element, read as far as the file holds it.
    Loaded,
    /// Clusters found by walking the file, as far as `walk` has got.
    Scan { walk: Walk, done: bool },
}

/// The points seeks land on, in timeline order.
pub(super) struct Index {
    points: Vec<Point>,
    source: Source,
}

impl Index {
    /// The index of `file`, read from its `Cues` element at `cues` where it
    /// names or holds one.
    pub(super) fn new(file: &Opened, cues: Option<u64>) -> Self {
        let source = match cues {
            Some(at) => Source::Cues { at, read: None },
            None => Source::Scan {
                walk: Walk::at(file.body),
                done: false,
            },
        };
        Self {
            points: Vec::new(),
            source,
        }
    }

    /// Whether the points come from the file's own index.
    pub(super) const fn read_from_cues(&self) -> bool {
        !matches!(self.source, Source::Scan { .. })
    }

    /// The Cluster the latest point at or before `target` names.
    pub(super) fn landing(&self, target: u64) -> Option<u64> {
        let after = self.points.partition_point(|point| point.tick <= target);
        let last = after.checked_sub(1)?;
        self.points.get(last).map(|point| point.cluster)
    }

    /// Drop the file's index, so seeks walk the file instead.
    pub(super) fn drop_cues(&mut self, file: &Opened) {
        self.points.clear();
        self.source = Source::Scan {
            walk: Walk::at(file.body),
            done: false,
        };
    }

    /// Hold every point a seek to `target` needs: the whole of the file's
    /// index, or the Clusters as far as one past `target`.
    pub(super) fn fill(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        file: &Opened,
        target: u64,
        scratch: &mut Vec<u8>,
    ) -> Result<(), DemuxError> {
        if matches!(self.source, Source::Cues { .. }) {
            match self.load(input, file, scratch) {
                Ok(()) => {}
                Err(
                    refused @ (DemuxError::InputUnavailable
                    | DemuxError::InputFailed
                    | DemuxError::OutOfMemory),
                ) => return Err(refused),
                Err(_) => {
                    self.drop_cues(file);
                    return Err(DemuxError::BadIndex);
                }
            }
        }
        if matches!(self.source, Source::Loaded) && self.points.is_empty() {
            self.drop_cues(file);
        }
        self.scan(input, file, target)
    }

    /// Read the `Cues` element an element at a time, keeping each point as it
    /// is read.
    fn load(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        file: &Opened,
        scratch: &mut Vec<u8>,
    ) -> Result<(), DemuxError> {
        while let Source::Cues { at, read } = self.source {
            let file_len = input.len();
            self.source = match read {
                None => match cues_span(input, file, at, file_len)? {
                    Some(span) => Source::Cues {
                        at,
                        read: Some(span),
                    },
                    None => Source::Loaded,
                },
                Some((next, end)) => {
                    match self.cue_step(input, file, (next, end), file_len, scratch)? {
                        Some(after) => Source::Cues {
                            at,
                            read: Some((after, end)),
                        },
                        None => Source::Loaded,
                    }
                }
            };
        }
        Ok(())
    }

    /// Read the element of the `Cues` data at `next`, keeping the point it
    /// states: the offset after it, `None` where the index ends.
    fn cue_step(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        file: &Opened,
        (next, end): (u64, u64),
        file_len: u64,
        scratch: &mut Vec<u8>,
    ) -> Result<Option<u64>, DemuxError> {
        let stop = end.min(file.segment.limit(file_len));
        if next >= stop || self.points.len() as u64 >= u64::from(MAX_INDEX_POINTS) {
            return Ok(None);
        }
        let Some(head) = read_head(input, next, stop, file.widths)? else {
            return if stop >= file_len {
                Ok(None)
            } else {
                Err(DemuxError::Overrun)
            };
        };
        let data = next.checked_add(head.len).ok_or(DemuxError::Overrun)?;
        let size = known(head.size)?;
        let after = data
            .checked_add(size)
            .filter(|&after| after <= end)
            .ok_or(DemuxError::Overrun)?;
        if after > file_len {
            return Ok(None);
        }
        if head.id == id::CUE_POINT {
            let bytes = slurp(input, data, size, scratch)?;
            if let Some((tick, position)) = cue_point(bytes, file.widths, file.seek_track)? {
                self.admit(file, tick, position, file_len)?;
            }
        }
        Ok(Some(after))
    }

    /// Keep the point at `tick` whose Cluster is `position` bytes into the
    /// Segment: refused out of order or ahead of the first Cluster, and passed
    /// over where the file no longer reaches it.
    fn admit(
        &mut self,
        file: &Opened,
        tick: u64,
        position: u64,
        file_len: u64,
    ) -> Result<(), DemuxError> {
        let cluster = file
            .segment
            .data
            .checked_add(position)
            .ok_or(DemuxError::BadIndex)?;
        if cluster < file.body || self.points.last().is_some_and(|last| tick < last.tick) {
            return Err(DemuxError::BadIndex);
        }
        if cluster >= file_len {
            return Ok(());
        }
        if fallible::push(&mut self.points, Point { tick, cluster }) {
            Ok(())
        } else {
            Err(DemuxError::OutOfMemory)
        }
    }

    /// Walk the file's Clusters as far as one past `target`, keeping a point
    /// for each: a known-size Cluster is stepped over whole once its timestamp
    /// is read.
    fn scan(
        &mut self,
        input: &mut (impl SoundInput + ?Sized),
        file: &Opened,
        target: u64,
    ) -> Result<(), DemuxError> {
        while let Source::Scan {
            mut walk,
            done: false,
        } = self.source
        {
            if self.points.last().is_some_and(|point| point.tick > target) {
                return Ok(());
            }
            let mut done = false;
            match walk.step(input, file)? {
                Step::Timed { start, tick, end } => {
                    self.note(start, tick)?;
                    if let Some(end) = end {
                        walk = Walk::at(end);
                    }
                }
                Step::Block { element, .. } => walk.at = element.end,
                Step::Ended(_) => done = true,
                Step::Entered | Step::Skipped | Step::Left => {}
            }
            self.source = Source::Scan { walk, done };
        }
        Ok(())
    }

    /// Keep a point for the Cluster at `start` timed at `tick`, while the
    /// points stay in timeline order and within their bound.
    fn note(&mut self, start: u64, tick: u64) -> Result<(), DemuxError> {
        let ordered = self.points.last().is_none_or(|last| tick >= last.tick);
        let room = (self.points.len() as u64) < u64::from(MAX_INDEX_POINTS);
        let point = Point {
            tick,
            cluster: start,
        };
        if ordered && room && !fallible::push(&mut self.points, point) {
            return Err(DemuxError::OutOfMemory);
        }
        Ok(())
    }
}

/// The data of the `Cues` element at `at`, where it starts and ends; `None`
/// where the file stops before its header does.
fn cues_span(
    input: &mut (impl SoundInput + ?Sized),
    file: &Opened,
    at: u64,
    file_len: u64,
) -> Result<Option<(u64, u64)>, DemuxError> {
    let limit = file.segment.limit(file_len);
    if at >= limit {
        return Ok(None);
    }
    let Some(head) = read_head(input, at, limit, file.widths)? else {
        return Ok(None);
    };
    if head.id != id::CUES {
        return Err(DemuxError::BadIndex);
    }
    let data = at.checked_add(head.len).ok_or(DemuxError::BadIndex)?;
    let end = data
        .checked_add(known(head.size)?)
        .ok_or(DemuxError::BadIndex)?;
    Ok(Some((data, end)))
}

/// A cue point's time and its Cluster's position for `track`, `None` where it
/// places no Cluster of `track`.
fn cue_point(data: &[u8], widths: Widths, track: u64) -> Result<Option<(u64, u64)>, DemuxError> {
    let mut time = None;
    let mut position = None;
    for child in Children::new(data, widths) {
        let (element, value) = child?;
        match element {
            id::CUE_TIME => once(&mut time, uint(value)?)?,
            id::CUE_TRACK_POSITIONS => {
                let (cue_track, cluster) = track_position(value, widths)?;
                if cue_track == track && position.is_none() {
                    position = Some(cluster);
                }
            }
            _ => {}
        }
    }
    let time = time.ok_or(DemuxError::Missing)?;
    Ok(position.map(|cluster| (time, cluster)))
}

/// The track a cue point's track positions name, and its Cluster's position.
fn track_position(data: &[u8], widths: Widths) -> Result<(u64, u64), DemuxError> {
    let mut track = None;
    let mut cluster = None;
    for child in Children::new(data, widths) {
        let (element, value) = child?;
        match element {
            id::CUE_TRACK => once(&mut track, uint(value)?)?,
            id::CUE_CLUSTER_POSITION => once(&mut cluster, uint(value)?)?,
            _ => {}
        }
    }
    Ok((
        track.ok_or(DemuxError::Missing)?,
        cluster.ok_or(DemuxError::Missing)?,
    ))
}
