//! A handle resolved for measuring and drawing: its font's data, loaded
//! again where the memory policy let the store's copy go, and the size and
//! matrix it was found at. A master measures as its font at one point.

use alloc::rc::Rc;

use tairix_riscos_kernel::os::{Fail, Os};

use crate::find::{check_range, load_data};
use crate::fontdata::FontData;
use crate::handles::Master;
use crate::identifier::Matrix;
use crate::module::{error, raise, with};
use crate::transform::Transform;
use crate::units;

/// A handle's font, ready to measure.
#[derive(Clone, Debug)]
pub(crate) struct Font {
    pub(crate) handle: u32,
    /// Its master's serial: whose data it is.
    pub(crate) serial: u64,
    pub(crate) data: Rc<FontData>,
    /// Its size across and up, in sixteenths of a point.
    pub(crate) points: (i32, i32),
    /// The dots an inch it is drawn at, across and up.
    pub(crate) resolution: (i32, i32),
    /// Its own matrix, the identity where it has none.
    pub(crate) matrix: Transform,
    /// Whether it was found with a matrix of its own.
    pub(crate) transformed: bool,
}

/// What a handle holds, read out of the table.
struct Read {
    master: Option<Master>,
    serial: u64,
    held: Option<Rc<FontData>>,
    points: (i32, i32),
    resolution: (i32, i32),
    matrix: Option<Matrix>,
}

/// Handle `handle`'s font: "Font handle out of range" outside 1 to 255,
/// "Undefined font handle" for one that holds none.
pub(crate) fn font(os: &mut Os<'_>, handle: u32) -> Result<Font, Fail> {
    check_range(os, handle)?;
    let read = with(os, |state| {
        let held = state.handles.held(handle)?;
        let serial = held.master.serial;
        let data = state.store.get(serial);
        let (points, resolution, matrix) = held.size.map_or(
            ((units::SIXTEENTHS, units::SIXTEENTHS), (0, 0), None),
            |s| (s.points, s.resolution, s.matrix),
        );
        Some(Read {
            master: data.is_none().then(|| held.master.clone()),
            serial,
            held: data,
            points,
            resolution,
            matrix,
        })
    })?;
    let Some(read) = read else {
        return Err(raise(os, error::UNDEFINED, &[]));
    };
    let data = match (read.held, read.master) {
        (Some(data), _) => data,
        (None, Some(master)) => {
            let data = load_data(os, &master)?;
            with(os, |state| state.store.keep(read.serial, &data))?;
            data
        }
        (None, None) => return Err(raise(os, error::UNDEFINED, &[])),
    };
    Ok(Font {
        handle,
        serial: read.serial,
        data,
        points: read.points,
        resolution: read.resolution,
        matrix: read.matrix.map_or(Transform::IDENTITY, Transform::of),
        transformed: read.matrix.is_some(),
    })
}

/// The current font's handle.
pub(crate) fn current(os: &mut Os<'_>) -> Result<u32, Fail> {
    with(os, |state| state.current.font)
}
