//! A pipeline as the software device runs it: each stage's program, lowered
//! once when the pipeline is made, and the tables a draw reads.

use alloc::vec::Vec;

use tairix_spirv::{EntryPoint, Interpolation};

use super::dispatch::{self, Need};
use super::shader::ir::{Carries, Fixed, Program, Term, LANES};
use super::shader::lower;
use crate::backend::Prepared;
use crate::binding::PipelineLayoutState;
use crate::format::Format;
use crate::info::{DeviceInfo, Features, Limits};
use crate::pipeline::StagePlan;
use crate::render::{holding, writes_memory, RenderPlan};
use crate::{Error, MAX_PASS_ATTACHMENTS};

use super::Software;

/// Where a vertex input port's words come from: an attribute of a vertex
/// buffer, read from its `channel`, narrowed to 16 bits where the port is.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Fetch {
    pub(crate) port: u32,
    pub(crate) buffer: u32,
    pub(crate) format: Format,
    pub(crate) offset: u32,
    pub(crate) channel: u32,
    pub(crate) narrow: bool,
}

/// Where a word of a fragment input comes from — a word of the shaded
/// vertex, or nothing the vertex stage wrote — and how it varies across its
/// primitive.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Link {
    pub(crate) word: Option<u32>,
    pub(crate) interpolation: Interpolation,
    pub(crate) centroid: bool,
    /// Whether it is 16 bits wide.
    pub(crate) narrow: bool,
}

tairix_util::flags! {
    /// What a fragment stage does that decides when its tests may run.
    pub(crate) struct Effects(u8) {
        /// Its fragments are tested before it runs, whatever it does.
        const EARLY = 1;
        /// It may discard.
        const DISCARDS = 1 << 1;
        /// It writes storage, images or atomics.
        const WRITES_MEMORY = 1 << 2;
        /// It writes its own depth.
        const WRITES_DEPTH = 1 << 3;
        /// It writes its own coverage.
        const WRITES_MASK = 1 << 4;
    }
}

/// A render pipeline's fragment stage: its program, where each word of its
/// located inputs comes from, the channels it writes of each target and of
/// the second colour a dual-source blend reads, and what decides when its
/// tests may run.
#[derive(Debug)]
pub(crate) struct Shading {
    pub(crate) program: Program,
    /// One per word of its located input ports, in their order, and the
    /// first of each input port's: none for a built-in's.
    pub(crate) links: Vec<Link>,
    pub(crate) starts: Vec<u32>,
    /// Which of its output ports, by place, are 16 bits wide.
    pub(crate) narrow_outputs: u64,
    /// The channels, red at bit 0, each target is written in; a target of
    /// none is left as it was.
    pub(crate) channels: [u8; MAX_PASS_ATTACHMENTS],
    pub(crate) second_channels: u8,
    pub(crate) effects: Effects,
}

/// A render pipeline's programs and the tables a draw reads of them.
#[derive(Debug)]
pub(crate) struct Render {
    pub(crate) vertex: Program,
    pub(crate) fragment: Option<Shading>,
    pub(crate) fetches: Vec<Fetch>,
    /// The words a shaded vertex holds — each output port's in turn, from
    /// its start — and where its position and point size lie among them.
    pub(crate) record: u32,
    pub(crate) starts: Vec<u32>,
    pub(crate) position: Option<u32>,
    pub(crate) point_size: Option<u32>,
    /// The largest side a point takes.
    pub(crate) largest_point: f64,
}

impl Render {
    /// The workspace a draw of it shades in: either stage's tile.
    fn need(&self) -> Need {
        let vertex = dispatch::need(&self.vertex);
        self.fragment.as_ref().map_or(vertex, |shading| {
            vertex.max(dispatch::need(&shading.program))
        })
    }

    /// The host memory it keeps beside its working memory.
    fn held_bytes(&self) -> u64 {
        use tairix_util::fallible::held;
        [
            self.vertex.held_bytes(),
            self.fragment.as_ref().map_or(0, |shading| {
                shading
                    .program
                    .held_bytes()
                    .saturating_add(held(&shading.links))
                    .saturating_add(held(&shading.starts))
            }),
            held(&self.fetches),
            held(&self.starts),
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

impl Software {
    /// A compute pipeline's program, its working memory and the host bytes it
    /// keeps.
    pub(crate) fn compute_form(
        info: &DeviceInfo,
        stage: &StagePlan<'_>,
    ) -> Result<Prepared<Self, Program>, Error> {
        let invocations = stage
            .workgroup
            .iter()
            .fold(1u64, |n, &s| n.saturating_mul(u64::from(s)));
        if invocations > (dispatch::MAX_TILES * LANES) as u64 {
            return Err(Error::TooLarge);
        }
        let program = lower_program(&info.limits, stage)?;
        Ok(Prepared {
            need: dispatch::need(&program),
            held: program.held_bytes(),
            form: program,
        })
    }

    /// A render pipeline's programs and tables, its working memory and the
    /// host bytes it keeps.
    pub(crate) fn render_form(
        info: &DeviceInfo,
        plan: &RenderPlan<'_>,
    ) -> Result<Prepared<Self, Render>, Error> {
        let vertex = lower_program(&info.limits, &plan.vertex)?;
        let fragment = plan
            .fragment
            .as_ref()
            .map(|stage| lower_program(&info.limits, stage))
            .transpose()?;
        let fetches = fetches(plan, &vertex)?;
        let mut record = 0u32;
        let (mut position, mut point_size) = (None, None);
        let mut starts = Vec::new();
        starts
            .try_reserve_exact(vertex.outputs.len())
            .map_err(|_| Error::OutOfMemory)?;
        for port in &vertex.outputs {
            match port.carries {
                Carries::Fixed(Fixed::Position) => position = Some(record),
                Carries::Fixed(Fixed::PointSize) => point_size = Some(record),
                _ => {}
            }
            starts.push(record);
            record = record.checked_add(port.words).ok_or(Error::TooLarge)?;
        }
        let fragment = match (fragment, &plan.fragment) {
            (Some(program), Some(stage)) => {
                Some(shading(plan, (&vertex, &starts), (program, stage.entry))?)
            }
            _ => None,
        };
        let largest_point = if info.features.contains(Features::LARGE_POINTS) {
            f64::from(info.limits.max_point_size)
        } else {
            1.0
        };
        let render = Render {
            vertex,
            fragment,
            fetches,
            record,
            starts,
            position,
            point_size,
            largest_point,
        };
        Ok(Prepared {
            need: render.need(),
            held: render.held_bytes(),
            form: render,
        })
    }
}

/// The program `stage` lowers to, its bindings placed among its layout's
/// sets' descriptors, checked against `limits`.
fn lower_program(limits: &Limits, stage: &StagePlan<'_>) -> Result<Program, Error> {
    let mut program = lower::lower(
        stage.module,
        stage.entry,
        &stage.specialized,
        stage.workgroup,
    )?;
    place_bindings(&mut program, stage.layout)?;
    let per_lane = u64::from(program.varying_words) + u64::from(program.private_words);
    let per_tile = u64::from(program.uniform_words)
        .saturating_sub(program.constants.len() as u64)
        .saturating_add(u64::from(program.shared_words));
    let memory = u64::from(limits.max_invocation_memory);
    if per_lane * 4 > memory
        || per_tile * 4 > memory
        || u64::from(program.workgroup_words) * 4 > u64::from(limits.max_workgroup_memory)
    {
        return Err(Error::TooLarge);
    }
    Ok(program)
}

/// Each of `program`'s bindings placed among the descriptors of the bind set
/// its layout takes there, and their total: fixed for every set a dispatch
/// binds, as each serves its layout's set.
fn place_bindings(program: &mut Program, layout: &PipelineLayoutState) -> Result<(), Error> {
    let mut descriptors = 0u64;
    for binding in &mut program.bindings {
        let set = layout
            .sets
            .get(binding.set as usize)
            .ok_or(Error::Malformed)?;
        let (_, first) = set.find(binding.number).ok_or(Error::Malformed)?;
        binding.first = u32::try_from(first).map_err(|_| Error::Malformed)?;
        descriptors = descriptors.saturating_add(u64::from(binding.count));
    }
    program.descriptors = descriptors;
    Ok(())
}

/// Where each of `vertex`'s located input ports is fed from: the attribute of
/// one of `plan`'s vertex buffers at its location, which the model has proved
/// is there and of its component type.
fn fetches(plan: &RenderPlan<'_>, vertex: &Program) -> Result<Vec<Fetch>, Error> {
    let entry = plan.vertex.entry;
    let attribute_at = |location: u32| {
        plan.state
            .vertex_buffers
            .iter()
            .zip(0u32..)
            .find_map(|(buffer, index)| {
                buffer
                    .attributes
                    .iter()
                    .find(|attribute| attribute.location == location)
                    .map(|attribute| (index, attribute))
            })
    };
    let mut fetches = Vec::new();
    fetches
        .try_reserve_exact(vertex.inputs.len())
        .map_err(|_| Error::OutOfMemory)?;
    for (port, index) in vertex.inputs.iter().zip(0u32..) {
        let Carries::Located {
            location,
            component,
            ..
        } = port.carries
        else {
            continue;
        };
        let (buffer, attribute) = attribute_at(location).ok_or(Error::Interface)?;
        let input = holding(entry.inputs(), location, component).ok_or(Error::Malformed)?;
        fetches.push(Fetch {
            port: index,
            buffer,
            format: attribute.format,
            offset: attribute.offset,
            channel: component,
            narrow: input.width == 16,
        });
    }
    Ok(fetches)
}

/// The fragment stage of a pipeline whose vertex stage is `vertex`, its
/// output ports starting at `starts`: where each word of its inputs comes
/// from, and what decides when its tests may run.
fn shading(
    plan: &RenderPlan<'_>,
    (vertex, starts): (&Program, &[u32]),
    (program, entry): (Program, &EntryPoint),
) -> Result<Shading, Error> {
    let source = |location: u32, component: u32| {
        vertex
            .outputs
            .iter()
            .zip(starts)
            .find_map(|(port, &start)| match port.carries {
                Carries::Located {
                    location: held,
                    component: first,
                    ..
                } if held == location
                    && (first..first.saturating_add(port.words)).contains(&component) =>
                {
                    Some(start + (component - first))
                }
                _ => None,
            })
    };
    let (links, firsts) = links_of(&program, entry, source)?;
    let narrow_outputs = narrow_outputs(&program, entry)?;
    let writes = |which: Fixed| {
        program
            .outputs
            .iter()
            .any(|port| port.carries == Carries::Fixed(which))
    };
    let effects = [
        (entry.early_fragment_tests(), Effects::EARLY),
        (
            program.blocks.iter().any(|block| block.term == Term::Kill),
            Effects::DISCARDS,
        ),
        (writes_memory(entry), Effects::WRITES_MEMORY),
        (writes(Fixed::FragDepth), Effects::WRITES_DEPTH),
        (writes(Fixed::SampleMask), Effects::WRITES_MASK),
    ]
    .into_iter()
    .filter(|&(holds, _)| holds)
    .fold(Effects::NONE, |all, (_, effect)| all.with(effect));
    Ok(Shading {
        effects,
        links,
        starts: firsts,
        narrow_outputs,
        channels: plan.channels,
        second_channels: plan.second_channels,
        program,
    })
}

/// Where each word of `program`'s located inputs comes from — `source`
/// finding the vertex output word at a location's component — and the first
/// link of each input port: none for a built-in's.
fn links_of(
    program: &Program,
    entry: &EntryPoint,
    source: impl Fn(u32, u32) -> Option<u32>,
) -> Result<(Vec<Link>, Vec<u32>), Error> {
    let words = program
        .inputs
        .iter()
        .fold(0usize, |total, port| match port.carries {
            Carries::Located { .. } => total.saturating_add(port.words as usize),
            Carries::Fixed(_) => total,
        });
    let mut links = Vec::new();
    links
        .try_reserve_exact(words)
        .map_err(|_| Error::OutOfMemory)?;
    let mut firsts = Vec::new();
    firsts
        .try_reserve_exact(program.inputs.len())
        .map_err(|_| Error::OutOfMemory)?;
    for port in &program.inputs {
        let Carries::Located {
            location,
            component,
            ..
        } = port.carries
        else {
            firsts.push(u32::MAX);
            continue;
        };
        firsts.push(u32::try_from(links.len()).map_err(|_| Error::TooLarge)?);
        for word in 0..port.words {
            let component = component + word;
            let input = holding(entry.inputs(), location, component).ok_or(Error::Malformed)?;
            links.push(Link {
                word: source(location, component),
                interpolation: input.interpolation,
                centroid: input.centroid,
                narrow: input.width == 16,
            });
        }
    }
    Ok((links, firsts))
}

/// Which of `program`'s output ports, by place, are 16 bits wide.
fn narrow_outputs(program: &Program, entry: &EntryPoint) -> Result<u64, Error> {
    let mut narrow_outputs = 0u64;
    for (port, place) in program.outputs.iter().zip(0u32..) {
        if let Carries::Located {
            location,
            component,
            index,
        } = port.carries
        {
            let narrow = entry.outputs().iter().any(|v| {
                v.location == location
                    && v.component == component
                    && v.index == index
                    && v.width == 16
            });
            if narrow {
                narrow_outputs |= 1u64.checked_shl(place).ok_or(Error::TooLarge)?;
            }
        }
    }
    Ok(narrow_outputs)
}
