//! Work that grows with the selection, done off the event loop: boolean
//! operations, objects turned into paths, strokes into outlines. It runs on
//! the application's queue, where text's glyphs can be fetched, and lands as
//! one step only if the document is still the one it was worked out from.

use alloc::vec::Vec;

use tairix_bezier::boolean::Operation;
use tairix_drawing::{Drawing, Glyphs, Ids, NodePath};

use crate::edit::{self, EditError};

/// How the selection is reshaped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Reshape {
    /// Combine the selection into one path.
    Combine(Operation),
    /// Turn every shape and text into the path it traces.
    ToPaths,
    /// Turn every stroke into the outline it paints.
    OutlineStrokes,
}

impl Reshape {
    /// What the step it makes is called.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Combine(Operation::Union) => "Union",
            Self::Combine(Operation::Intersection) => "Intersection",
            Self::Combine(Operation::Difference) => "Difference",
            Self::Combine(Operation::Exclusion) => "Exclusion",
            Self::ToPaths => "Object to path",
            Self::OutlineStrokes => "Outline stroke",
        }
    }
}

/// A job for the queue.
#[derive(Clone, Debug)]
pub struct Compute {
    /// Its number, to match its answer.
    pub job: u64,
    /// The document's generation it is worked out from.
    pub generation: u64,
    /// The drawing then.
    pub drawing: Drawing,
    /// The objects it works on.
    pub selection: Vec<NodePath>,
    /// What to do to them.
    pub operation: Reshape,
    /// Where new objects' identities come from.
    pub ids: Ids,
}

/// What a job came to.
#[derive(Debug)]
pub struct Computed {
    /// The job's number.
    pub job: u64,
    /// The generation it was worked out from.
    pub generation: u64,
    /// What it made: the drawing, the objects to select, and the identities
    /// issued so far.
    pub result: Result<(Drawing, Vec<NodePath>, Ids), EditError>,
    /// What the step is called.
    pub label: &'static str,
}

/// Work `compute` out, text set from `glyphs`.
#[must_use]
pub fn work_out(compute: Compute, glyphs: &dyn Glyphs) -> Computed {
    let Compute {
        job,
        generation,
        drawing,
        selection,
        operation,
        mut ids,
    } = compute;
    let result = match operation {
        Reshape::Combine(operation) => {
            edit::combine(&drawing, &selection, operation, glyphs, &mut ids)
                .map(|(drawing, path)| (drawing, alloc::vec![path], ids))
        }
        Reshape::ToPaths => {
            edit::to_paths(&drawing, &selection, glyphs).map(|drawing| (drawing, selection, ids))
        }
        Reshape::OutlineStrokes => {
            edit::outline_strokes(&drawing, &selection).map(|drawing| (drawing, selection, ids))
        }
    };
    Computed {
        job,
        generation,
        result,
        label: operation.label(),
    }
}
