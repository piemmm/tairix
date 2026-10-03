//! Grid fitting: snapping an outline onto the pixel grid before it is filled.
//!
//! A face places its stems and its x-height where the *design* wants them,
//! which at a text-console cell size is almost never a whole pixel. A stem
//! 1.2 pixels wide straddling two columns fills both at around 60%, so a
//! screen of letters comes out as grey haze rather than type: measured on the
//! committed console face at its 8×16 cell, fewer than one inked pixel in ten
//! reached full coverage and nearly half sat at mid-grey. A face carrying
//! TrueType hinting bytecode fixes this itself; the committed faces do not
//! (`Inconsolata-EX` ships a 7-byte stub `prep` and no `fpgm`/`cvt`), so the
//! fitting is synthesised from the outline — the same reason FreeType carries
//! an autofitter, and broadly its algorithm.
//!
//! The fit is a **monotone piecewise-linear warp of one coordinate**. Along
//! each axis the outline's near-axis segments are clustered into [`Edge`]s,
//! adjacent edges with ink between them are paired into strokes, each stroke
//! is snapped to a whole number of pixels at a whole pixel position, and every
//! outline coordinate is then interpolated between those snapped controls.
//! Nothing else moves: a diagonal stays a straight diagonal because its
//! segments never register as an edge, and ordering is preserved so a fitted
//! outline never turns inside out.
//!
//! Rows additionally snap to the face's [`FitMetrics`] zones — its baseline,
//! x-height, cap height, ascender and descender, read off the face's own
//! reference glyphs — so every letter of a line agrees on where those lines
//! fall instead of each rounding independently. A round letter's overshoot is
//! flattened onto the zone only while it is worth less than a pixel, which is
//! exactly when drawing it would cost a whole row. A stroke with no zone of
//! its own is placed between the zone-held ones, and every bar near the face's
//! standard thickness is drawn at the standard's width, so the bars of a line
//! agree on their weight as well as their rows.
//!
//! Columns are fitted by transposing and running the same routine, so there
//! is one implementation of the algorithm rather than a mirrored copy.
//! Columns are snapped only for a fixed cell ([`Axes::RowsAndColumns`]): the
//! cell owns the advance, so moving a stem costs no spacing, and the ink is
//! kept inside the cells the glyph occupies. Proportional
//! text is fitted along rows alone ([`Axes::Rows`]), because a snapped column
//! would move ink out from under the unsnapped advance that laid the run out.

use alloc::vec;
use alloc::vec::Vec;

use crate::engine::{crossing, Segment};
use tairix_util::mathf;

/// The steepest slope, as a fraction, a segment may run off an axis and still
/// read as an edge along it: one in four, about 76° from the axis.
///
/// Anything steeper is a diagonal — the stroke of an `x`, the leg of an `A` —
/// which must stay a straight diagonal rather than be snapped into a
/// staircase.
const EDGE_SLOPE: f64 = 0.25;

/// Pixels within which two parallel segments read as one edge, and within
/// which a single segment counts as lying *on* a line rather than crossing it.
///
/// A curve reaches its extremum over several chords, so the flat of an `o`
/// arrives as a short run of nearly-parallel segments a fraction of a pixel
/// apart. They are one edge.
///
/// The same tolerance bounds how far one segment may wander: a long shallow
/// run passes the slope test while still drifting pixels across, and the arms
/// of a `w` are exactly that. Snapping such a run onto a column does not
/// sharpen a stem, it shears a diagonal, so an edge must be flat in absolute
/// terms and not merely shallow.
const EDGE_FUZZ: f64 = 0.35;

/// How far a segment may wander and still be fitted as one side of a stroke
/// whose other side is firm. The sides of a short slanted stroke — a leg of
/// `#` between its bars — wander equally, so one cut-off can snap one side and
/// leave the other a half-covered diagonal; a lone diagonal is never admitted.
const FACING_EDGE_FUZZ: f64 = 0.5;

/// Pixels either side of an alignment zone within which an edge belongs to it.
const ZONE_FUZZ: f64 = 0.4;

/// How far, as a fraction of the face's standard bar, a stroke may stray and
/// still be drawn at the standard's width: enough to catch bars drawn alike,
/// not enough to darken the lowercase `e`'s deliberately lighter one.
const STANDARD_FUZZ: f64 = 0.08;

/// Edges along one axis beyond which an outline is filled unfitted.
///
/// Pairing edges into strokes probes the outline once per edge, so the work
/// is the edge count times the segment count: linear for a glyph, quadratic
/// for an adversarial one whose every segment is a separate flat. This caps
/// it at the rasteriser's own order and is a bound on untrusted input, not a
/// capacity to grow — the densest scripts reach a few dozen edges an axis, so
/// nothing that is a glyph comes near it, and anything that does still draws,
/// just without the cosmetic fitting.
const MAX_EDGES: usize = 256;

/// One alignment zone, in font units measured **downward from the baseline**
/// (so a zone above the baseline is negative), the way the outline sink
/// measures y.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Zone {
    /// Where flat-sided glyphs stop: the x-height of `x`, the cap height of
    /// `H`, the baseline itself.
    flat: f64,
    /// Where round glyphs overshoot past that: the top of `o`, the bottom of
    /// `O`. Equal to `flat` when the face has no round reference.
    over: f64,
}

impl Zone {
    /// A zone flat-sided glyphs reach at `flat` and round ones overshoot to
    /// at `over`.
    pub(crate) const fn new(flat: f64, over: f64) -> Self {
        Self { flat, over }
    }
}

/// What every glyph of a face is fitted against, read off its own outlines
/// once when it is parsed.
#[derive(Clone, Debug, Default)]
pub(crate) struct FitMetrics {
    zones: Vec<Zone>,
    /// How thick the face draws its horizontal strokes, in font units.
    bar: Option<f64>,
}

impl FitMetrics {
    /// The alignment `zones`, in no particular order, and the `bar` thickness
    /// the face's horizontal strokes share, when it has one.
    pub(crate) const fn new(zones: Vec<Zone>, bar: Option<f64>) -> Self {
        Self { zones, bar }
    }

    /// Resolve every zone to the pixel row it snaps to, for an outline whose
    /// baseline sits at `baseline` and whose font units scale by `scale`.
    ///
    /// The band an edge must fall in to belong to a zone spans the overshoot
    /// only while the overshoot is worth less than a whole pixel: below that
    /// it cannot be drawn without costing a full row, above it the design's
    /// overshoot is real and is left alone.
    fn bands(&self, baseline: f64, scale: f64) -> Vec<Band> {
        self.zones
            .iter()
            .map(|zone| {
                let flat = baseline + zone.flat * scale;
                let over = baseline + zone.over * scale;
                let (lo, hi) = if mathf::fabs(over - flat) < 1.0 {
                    (mathf::fmin(flat, over), mathf::fmax(flat, over))
                } else {
                    (flat, flat)
                };
                Band {
                    lo: lo - ZONE_FUZZ,
                    hi: hi + ZONE_FUZZ,
                    to: mathf::round(flat),
                }
            })
            .collect()
    }
}

/// A zone resolved to pixel space: the band of rows that belong to it and the
/// row they all snap to.
struct Band {
    lo: f64,
    hi: f64,
    to: f64,
}

/// Which axes an outline is snapped along.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Axes {
    /// Rows only. Columns keep their exact positions, so advances and side
    /// bearings are untouched — what proportional text needs, since ink
    /// snapped out from under its advance would drift from the run that was
    /// laid out.
    Rows,
    /// Rows and columns, for a fixed cell: the cell already owns the advance,
    /// so snapping a stem to a column costs no spacing and is what turns it
    /// into one solid column instead of two grey ones. `span`, when the face
    /// fixes one, is the pixel width of the glyph's cells; its ink is kept
    /// inside them, since anything past their edge lands in the next character.
    RowsAndColumns { span: Option<f64> },
}

/// Snap `segments` onto the pixel grid.
///
/// `baseline` is the pixel row the outline's baseline sits on and `scale` the
/// font-units-to-pixels factor its rows were laid out with, together placing
/// `metrics` in the same space as the segments.
pub(crate) fn fit(
    segments: &mut [Segment],
    axes: Axes,
    metrics: &FitMetrics,
    baseline: f64,
    scale: f64,
) {
    if segments.is_empty() {
        return;
    }
    fit_rows(
        segments,
        &metrics.bands(baseline, scale),
        metrics.bar.map(|bar| bar * scale),
        None,
    );
    if let Axes::RowsAndColumns { span } = axes {
        transpose(segments);
        fit_rows(segments, &[], None, span);
        transpose(segments);
    }
}

/// Exchange the axes of every segment, so the row fitter reads the columns.
fn transpose(segments: &mut [Segment]) {
    for segment in segments {
        core::mem::swap(&mut segment.x0, &mut segment.y0);
        core::mem::swap(&mut segment.x1, &mut segment.y1);
    }
}

/// Snap the rows of `segments`, holding edges that fall in a `bands` zone to
/// that zone's row, drawing strokes near the `standard` thickness (in pixels)
/// at its width, and keeping everything within `limit` rows of zero when one
/// is given.
fn fit_rows(segments: &mut [Segment], bands: &[Band], standard: Option<f64>, limit: Option<f64>) {
    let edges = edges(segments);
    if edges.is_empty() || edges.len() > MAX_EDGES {
        return;
    }
    let strokes = strokes(&edges, segments);
    let controls = controls(&edges, &strokes, bands, standard, limit);
    warp(segments, &controls);
    if let Some(limit) = limit {
        for segment in segments {
            segment.y0 = mathf::fmin(mathf::fmax(segment.y0, 0.0), limit);
            segment.y1 = mathf::fmin(mathf::fmax(segment.y1, 0.0), limit);
        }
    }
}

/// One near-horizontal segment, before it is clustered into an edge.
struct Flat {
    /// The row it lies on: the middle of the rows it wanders across.
    pos: f64,
    /// The rows it actually occupies, top and bottom.
    top: f64,
    bottom: f64,
    /// The columns it spans.
    lo: f64,
    hi: f64,
    /// Which way the outline travels along it.
    dir: i32,
}

/// A run of near-horizontal segments at essentially one row.
struct Edge {
    /// The row the edge is taken to lie on: its segments' length-weighted
    /// mean.
    pos: f64,
    /// The rows its segments actually occupy. They all move onto the fitted
    /// row together, so a stroke's boundary comes out straight and lands
    /// whole on one pixel instead of overhanging the next by the fraction it
    /// wandered.
    begin: f64,
    end: f64,
    /// The columns it covers, ascending and disjoint.
    ///
    /// An edge is rarely one run: the top of an `m` is the left stem's flat
    /// and the two arch crowns, with the valley over the middle stem between
    /// them, and the top of a `g` is its bowl's crown and its ear. Keeping
    /// the runs rather than their span is what lets [`shared_column`] probe a
    /// column the edge really occupies.
    runs: Vec<(f64, f64)>,
    /// Which way the outline travels along the edge. The two sides of a
    /// stroke are always traversed in opposite senses, so an edge never
    /// gathers segments from both: without this a stroke thinner than
    /// [`EDGE_FUZZ`] — a rule, a light face's hairline — would read as one
    /// edge and be snapped out of existence.
    dir: i32,
    /// Whether its segments stay within [`EDGE_FUZZ`]. One that only stays
    /// within [`FACING_EDGE_FUZZ`] is fitted solely as the facing side of a
    /// stroke whose other side does.
    firm: bool,
}

/// Cluster the near-horizontal segments of `segments` into edges, ordered by
/// ascending row.
///
/// A cluster gathers the segments running the same way within [`EDGE_FUZZ`]
/// of the first row it saw. Segments are visited in row order and a cluster
/// never reaches back, so each pass yields its edges in order. Firm and
/// merely facing segments are clustered apart, so admitting the second never
/// changes an edge the first would have formed.
fn edges(segments: &[Segment]) -> Vec<Edge> {
    let mut firm: Vec<Flat> = Vec::new();
    let mut facing: Vec<Flat> = Vec::new();
    for segment in segments {
        let run = segment.x1 - segment.x0;
        let rise = mathf::fabs(segment.y1 - segment.y0);
        if run == 0.0 || rise > mathf::fabs(run) * EDGE_SLOPE || rise > FACING_EDGE_FUZZ {
            continue;
        }
        let flat = Flat {
            pos: f64::midpoint(segment.y0, segment.y1),
            top: mathf::fmin(segment.y0, segment.y1),
            bottom: mathf::fmax(segment.y0, segment.y1),
            lo: mathf::fmin(segment.x0, segment.x1),
            hi: mathf::fmax(segment.x0, segment.x1),
            dir: if run > 0.0 { 1 } else { -1 },
        };
        if rise <= EDGE_FUZZ {
            firm.push(flat);
        } else {
            facing.push(flat);
        }
    }
    let mut edges = cluster(firm, true);
    edges.extend(cluster(facing, false));
    edges.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    edges
}

/// Cluster `flats` into edges, each marked `firm` or not.
fn cluster(mut flats: Vec<Flat>, firm: bool) -> Vec<Edge> {
    flats.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    let mut edges: Vec<Edge> = Vec::new();
    let mut first = 0.0;
    let mut weighted = 0.0;
    let mut weight = 0.0;
    for flat in flats {
        let run = flat.hi - flat.lo;
        match edges.last_mut() {
            Some(edge) if edge.dir == flat.dir && flat.pos - first <= EDGE_FUZZ => {
                weighted += flat.pos * run;
                weight += run;
                edge.pos = weighted / weight;
                edge.begin = mathf::fmin(edge.begin, flat.top);
                edge.end = mathf::fmax(edge.end, flat.bottom);
                edge.runs.push((flat.lo, flat.hi));
            }
            _ => {
                first = flat.pos;
                weighted = flat.pos * run;
                weight = run;
                edges.push(Edge {
                    pos: flat.pos,
                    begin: flat.top,
                    end: flat.bottom,
                    runs: vec![(flat.lo, flat.hi)],
                    dir: flat.dir,
                    firm,
                });
            }
        }
    }
    for edge in &mut edges {
        coalesce(&mut edge.runs);
    }
    edges
}

/// Sort `runs` by column and merge those that meet or overlap, leaving them
/// ascending and disjoint.
///
/// The chords of one curve arrive out of column order and share their
/// endpoints exactly, so they come back as the single run they draw.
fn coalesce(runs: &mut Vec<(f64, f64)>) {
    runs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut kept = 0;
    for index in 1..runs.len() {
        let (lo, hi) = runs[index];
        if lo <= runs[kept].1 {
            runs[kept].1 = mathf::fmax(runs[kept].1, hi);
        } else {
            kept += 1;
            runs[kept] = (lo, hi);
        }
    }
    runs.truncate(kept + 1);
}

/// The middle of the widest column interval both edges cover, or `None` when
/// they cover no column in common and so cannot bound a stroke between them.
///
/// Asking where the two edges *both* have material, rather than where their
/// spans happen to overlap, is what keeps the stroke probe out of a valley
/// like the one over an `m`'s middle stem: a probe there reads no ink, the
/// arch is not recognised as a stroke, and its two sides are then free to
/// close on each other. The widest interval is the one furthest from either
/// edge's ends, so it is the soundest place to ask.
fn shared_column(edge: &Edge, other: &Edge) -> Option<f64> {
    shared_runs(&edge.runs, &other.runs)
        .reduce(|widest, run| {
            if run.1 - run.0 > widest.1 - widest.0 {
                run
            } else {
                widest
            }
        })
        .map(|(lo, hi)| f64::midpoint(lo, hi))
}

/// The intervals two ascending, disjoint interval lists both cover, in order.
fn shared_runs<'a>(
    ours: &'a [(f64, f64)],
    theirs: &'a [(f64, f64)],
) -> impl Iterator<Item = (f64, f64)> + 'a {
    let (mut mine, mut yours) = (0, 0);
    core::iter::from_fn(move || {
        while let (Some(&(our_lo, our_hi)), Some(&(their_lo, their_hi))) =
            (ours.get(mine), theirs.get(yours))
        {
            if our_hi < their_hi {
                mine += 1;
            } else {
                yours += 1;
            }
            let (lo, hi) = (mathf::fmax(our_lo, their_lo), mathf::fmin(our_hi, their_hi));
            if hi > lo {
                return Some((lo, hi));
            }
        }
        None
    })
}

/// Two edges with the ink of one stroke between them.
struct Stroke {
    /// Index of the upper edge in the row-ordered edge list.
    lo: usize,
    /// Index of the lower edge.
    hi: usize,
}

/// Pair edges into the strokes they bound.
///
/// A stroke is bounded by its two *facing* sides, so an edge pairs only with
/// one the outline travels the opposite way along: the near side of a `j`'s
/// hook and the near side of its stem both face left, and reading them as a
/// stroke puts the stem's two sides on one column and erases it. Of the
/// edges that do face it, the nearest one it [shares a column](shared_column)
/// with is the candidate, and only when the outline is actually filled
/// between them — the counter of an `o` and the gap of an `m` are bounded by
/// facing edges too, and must not be mistaken for strokes. An edge that is
/// not [firm](Edge::firm) pairs only with a firm one, so two near-misses never
/// make a stroke. Narrower candidates are taken first, so where two pairings
/// compete the tighter (more certainly a stroke) one wins.
fn strokes(edges: &[Edge], segments: &[Segment]) -> Vec<Stroke> {
    let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
    for (index, edge) in edges.iter().enumerate() {
        let kinds: &[bool] = if edge.firm { &[true, false] } else { &[true] };
        for &firm in kinds {
            let facing = edges
                .iter()
                .enumerate()
                .skip(index + 1)
                .filter(|(_, other)| other.dir != edge.dir && other.firm == firm)
                .find_map(|(next, other)| {
                    shared_column(edge, other).map(|column| (next, other, column))
                });
            let Some((next, other, column)) = facing else {
                continue;
            };
            if winding(segments, column, f64::midpoint(edge.pos, other.pos)) != 0 {
                candidates.push((other.pos - edge.pos, index, next));
            }
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut taken = vec![false; edges.len()];
    let mut strokes = Vec::new();
    for (_, lo, hi) in candidates {
        if taken[lo] || taken[hi] {
            continue;
        }
        taken[lo] = true;
        taken[hi] = true;
        strokes.push(Stroke { lo, hi });
    }
    strokes
}

/// The winding number of the outline at `(column, row)`: non-zero is inside
/// the fill, by the same rule the rasteriser fills with.
fn winding(segments: &[Segment], column: f64, row: f64) -> i32 {
    segments
        .iter()
        .filter_map(|segment| crossing(segment, row))
        .filter(|&(at, _)| at < column)
        .map(|(_, direction)| direction)
        .sum()
}

/// Where each edge is moved to, as `(from, to)` rows ordered by `from`.
///
/// A stroke keeps a whole number of pixels of width, at least one, and one
/// near the face's `standard` takes the standard's, so bars drawn alike round
/// alike. An end in an alignment zone goes to the zone's row. A stroke with no
/// zone is centred between the zone rows either side, so the middle bars of
/// `E`, `F` and `H` agree and an `E`'s counters keep the design's proportion.
/// With no zones — a cell's columns — a stroke keeps its rounded design
/// distance to the nearest one placed alongside it, so evenly spaced stems (an
/// `m`'s, a `#`'s legs) stay evenly spaced; one with none alongside takes the
/// first one's shift.
///
/// An edge bounding no stroke takes its zone's row or moves with the fitted
/// edges around it, unrounded: rounding it onto a neighbour deletes the ink
/// between them.
fn controls(
    edges: &[Edge],
    strokes: &[Stroke],
    bands: &[Band],
    standard: Option<f64>,
    limit: Option<f64>,
) -> Vec<(f64, f64)> {
    let mut used: Vec<bool> = edges.iter().map(|edge| edge.firm).collect();
    for stroke in strokes {
        used[stroke.lo] = true;
        used[stroke.hi] = true;
    }
    let zoned: Vec<Option<f64>> = edges
        .iter()
        .zip(&used)
        .map(|(edge, &used)| {
            if used {
                zone_row(edge.pos, bands)
            } else {
                None
            }
        })
        .collect();
    let mut ordered: Vec<&Stroke> = strokes.iter().collect();
    ordered.sort_by_key(|stroke| stroke.lo);

    let mut to: Vec<Option<f64>> = vec![None; edges.len()];
    let mut free: Vec<&Stroke> = Vec::new();
    for stroke in ordered {
        let width = stroke_width(edges[stroke.hi].pos - edges[stroke.lo].pos, standard);
        let (lo, hi) = match (zoned[stroke.lo], zoned[stroke.hi]) {
            (Some(lo), Some(hi)) => (lo, mathf::fmax(hi, lo + 1.0)),
            (Some(lo), None) => (lo, lo + width),
            (None, Some(hi)) => (hi - width, hi),
            (None, None) => {
                free.push(stroke);
                continue;
            }
        };
        to[stroke.lo] = Some(lo);
        to[stroke.hi] = Some(hi);
    }
    let anchors = decided(edges, zoned.iter().copied());

    let mut shift: Option<f64> = None;
    let mut beside: Vec<Placed> = Vec::with_capacity(free.len());
    for stroke in free {
        let (from_lo, from_hi) = (edges[stroke.lo].pos, edges[stroke.hi].pos);
        let width = stroke_width(from_hi - from_lo, standard);
        let centre = f64::midpoint(from_lo, from_hi);
        let unfitted = centre - width * 0.5;
        let across: Vec<(f64, f64)> =
            shared_runs(&edges[stroke.lo].runs, &edges[stroke.hi].runs).collect();
        let covered = across.iter().map(|(lo, hi)| hi - lo).sum();
        let lo = if let Some(centre) = follow(&anchors, centre) {
            mathf::round(centre - width * 0.5)
        } else if let Some(near) = nearest_alongside(&beside, &across, covered, centre) {
            near.lo + mathf::round(unfitted - near.unfitted)
        } else {
            let lo = mathf::round(unfitted + shift.unwrap_or(0.0));
            shift = shift.or(Some(lo - unfitted));
            lo
        };
        let lo = limit.map_or(lo, |limit| mathf::fmax(0.0, mathf::fmin(lo, limit - width)));
        beside.push(Placed {
            across,
            covered,
            centre,
            unfitted,
            lo,
        });
        to[stroke.lo] = Some(lo);
        to[stroke.hi] = Some(lo + width);
    }
    let shift = shift.unwrap_or(0.0);
    let placed = if anchors.is_empty() {
        Vec::new()
    } else {
        decided(
            edges,
            to.iter().zip(&zoned).map(|(placed, zone)| placed.or(*zone)),
        )
    };

    let mut controls: Vec<(f64, f64)> = Vec::with_capacity(edges.len() * 2);
    let mut floor = f64::NEG_INFINITY;
    for (index, edge) in edges.iter().enumerate().filter(|&(index, _)| used[index]) {
        let row = to[index]
            .or(zoned[index])
            .or_else(|| follow(&placed, edge.pos))
            .unwrap_or(edge.pos + shift);
        // Zones and stroke widths are decided per edge, so hold the running
        // maximum: a warp that reordered two edges would fold the outline
        // over itself.
        floor = mathf::fmax(floor, row);
        // Both ends of the run, so every segment of the edge lands on the
        // fitted row rather than being interpolated towards it.
        control(&mut controls, edge.begin, floor);
        control(&mut controls, edge.end, floor);
    }
    controls
}

/// Add `(from, to)` to the controls, keeping them strictly ascending in
/// `from`: a control at a row already claimed folds into the one there.
fn control(controls: &mut Vec<(f64, f64)>, from: f64, to: f64) {
    match controls.last_mut() {
        Some(last) if from <= last.0 => last.1 = mathf::fmax(last.1, to),
        _ => controls.push((from, to)),
    }
}

/// The row an edge at `pos` snaps to for belonging to one of `bands`, or
/// `None` when it belongs to none.
fn zone_row(pos: f64, bands: &[Band]) -> Option<f64> {
    bands
        .iter()
        .find(|band| pos >= band.lo && pos <= band.hi)
        .map(|band| band.to)
}

/// A stroke already placed with no anchor to place it by.
struct Placed {
    /// The columns both of its edges cover, and how many that is.
    across: Vec<(f64, f64)>,
    covered: f64,
    centre: f64,
    /// Its low edge's design row for its fitted width.
    unfitted: f64,
    lo: f64,
}

/// The placed stroke nearest `centre` among those sharing at least half the
/// columns of the shorter of it and `across` (which covers `covered`) — the
/// strokes beside this one, rather than one merely touching it at a corner.
fn nearest_alongside<'a>(
    placed: &'a [Placed],
    across: &[(f64, f64)],
    covered: f64,
    centre: f64,
) -> Option<&'a Placed> {
    placed
        .iter()
        .filter(|other| {
            let shared: f64 = shared_runs(across, &other.across)
                .map(|(lo, hi)| hi - lo)
                .sum();
            let shorter = mathf::fmin(covered, other.covered);
            shorter > 0.0 && shared >= shorter * 0.5
        })
        .min_by(|a, b| mathf::fabs(a.centre - centre).total_cmp(&mathf::fabs(b.centre - centre)))
}

/// The whole number of pixels, never less than one, a stroke `span` pixels
/// thick is drawn at: the face's `standard` thickness rounded when the stroke
/// is within [`STANDARD_FUZZ`] of it, its own otherwise.
fn stroke_width(span: f64, standard: Option<f64>) -> f64 {
    let thickness = match standard {
        Some(standard) if mathf::fabs(span - standard) <= standard * STANDARD_FUZZ => standard,
        _ => span,
    };
    mathf::fmax(1.0, mathf::round(thickness))
}

/// The edges `rows` decides a row for, as `(from, to)` ascending in `from`.
fn decided(edges: &[Edge], rows: impl Iterator<Item = Option<f64>>) -> Vec<(f64, f64)> {
    let mut decided: Vec<(f64, f64)> = edges
        .iter()
        .zip(rows)
        .filter_map(|(edge, row)| row.map(|row| (edge.pos, row)))
        .collect();
    decided.sort_by(|a, b| a.0.total_cmp(&b.0));
    decided
}

/// Where a row at `pos` moves with the `fitted` edges either side of it:
/// between two it keeps its share of the gap, beyond them all it takes the
/// nearest one's move. `None` when nothing is fitted.
fn follow(fitted: &[(f64, f64)], pos: f64) -> Option<f64> {
    let split = fitted.partition_point(|&(from, _)| from <= pos);
    let before = split.checked_sub(1).and_then(|index| fitted.get(index));
    match (before, fitted.get(split)) {
        (Some(&(lo_from, lo_to)), Some(&(hi_from, hi_to))) => {
            Some(lo_to + (pos - lo_from) * (hi_to - lo_to) / (hi_from - lo_from))
        }
        (Some(&(from, to)), None) | (None, Some(&(from, to))) => Some(pos + (to - from)),
        (None, None) => None,
    }
}

/// Move every segment endpoint through the piecewise-linear map `controls`
/// describes: exactly onto each control, linearly interpolated between them,
/// and shifted by the nearest control's move beyond the ends.
fn warp(segments: &mut [Segment], controls: &[(f64, f64)]) {
    if controls.is_empty() {
        return;
    }
    for segment in segments {
        segment.y0 = map(controls, segment.y0);
        segment.y1 = map(controls, segment.y1);
    }
}

/// `row` through the piecewise-linear map `controls` describes.
///
/// Written so a coordinate sitting exactly on a control comes back as exactly
/// that control's row: the difference is a rounding step, but it is the
/// difference between a stroke boundary landing on the pixel edge and
/// overhanging it by a millionth, which the fill turns into a visible tint.
fn map(controls: &[(f64, f64)], row: f64) -> f64 {
    let (Some(&(first_from, first_to)), Some(&(last_from, last_to))) =
        (controls.first(), controls.last())
    else {
        return row;
    };
    if row <= first_from {
        return first_to - (first_from - row);
    }
    if row >= last_from {
        return last_to + (row - last_from);
    }
    for pair in controls.windows(2) {
        let [(lo_from, lo_to), (hi_from, hi_to)] = *pair else {
            continue;
        };
        if row < hi_from {
            // The controls strictly ascend in `from`, so the span is not zero.
            return lo_to + (row - lo_from) * (hi_to - lo_to) / (hi_from - lo_from);
        }
    }
    row
}
