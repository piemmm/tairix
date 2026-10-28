//! The stage interface: each built-in an entry point's inputs and outputs
//! hold, by the Vulkan environment's table, once apiece; each user-defined
//! one's locations and components, assigned, in range and not overlapping;
//! and the interpolation decorations the stage allows.

use alloc::vec::Vec;

use super::binding::Component;
use super::entry::{Stage, Varying};
use super::types::{Holds, Scalar, Ty};
use super::{push, Rule, Validator, MAX_LOCATIONS, NONE};
use crate::enumerant::{BuiltIn, Decoration, StorageClass};
use crate::Id;

/// One location slot taken: its location, component mask, blend index and
/// interpolation, and what it holds.
#[derive(Copy, Clone, Debug)]
struct Slot {
    location: u32,
    mask: u8,
    index: u32,
    interpolation: u8,
    varying: Varying,
}

/// What one side of a stage interface occupies: each location's components
/// under each blend index and, for fragment inputs, its interpolation — so an
/// overlap is refused as it is taken, and no side holds more slots than it
/// has components.
struct Occupancy {
    masks: [[u8; LOCATIONS]; 2],
    interpolation: [Option<u8>; LOCATIONS],
    fragment_input: bool,
    varyings: Vec<Varying>,
}

const LOCATIONS: usize = MAX_LOCATIONS as usize;

/// The interpolation decorations, as the bits a slot's interpolation is.
const FLAT: u8 = 1;
const NO_PERSPECTIVE: u8 = 1 << 1;
const CENTROID: u8 = 1 << 2;

impl Validator<'_> {
    /// Check the interface's built-ins and user-defined variables, and
    /// answer the locations its inputs and outputs occupy.
    pub(crate) fn stage_interface(
        &self,
        stage: Stage,
        interface: &[u32],
    ) -> Result<(Vec<Varying>, Vec<Varying>), Rule> {
        let mut built_ins: Vec<(u32, u32)> = Vec::new();
        let mut blocks = [false; 2];
        let mut sides = [
            Occupancy::new(stage == Stage::Fragment),
            Occupancy::new(false),
        ];
        for &slot in interface {
            let Some(&(id, _)) = self.m.definitions.get(slot as usize) else {
                continue;
            };
            let class = self.class_of(id)?;
            let side = match class {
                StorageClass::INPUT => 0,
                StorageClass::OUTPUT => 1,
                _ => continue,
            };
            let Ty::Pointer { pointee, .. } = self.ty(self.value_type(id)?).ty else {
                continue;
            };
            if let Some(built_in) = self.decoration_value_of(id, NONE, Decoration::BUILT_IN) {
                self.built_in(stage, class, BuiltIn(built_in), pointee)?;
                claim(&mut built_ins, (class.0, built_in))?;
                continue;
            }
            let block = Some(self.innermost(pointee))
                .filter(|&t| self.ty(t).holds.contains(Holds::BUILT_IN));
            if let Some(block) = block {
                if blocks
                    .get_mut(side)
                    .is_some_and(|b| core::mem::replace(b, true))
                {
                    return Err(Rule::BuiltIn);
                }
                let Ty::Struct { first, count } = self.ty(block).ty else {
                    return Err(Rule::BuiltIn);
                };
                let struct_id = self.ty(block).id.ok_or(Rule::BuiltIn)?;
                for (member, &member_ty) in self.list(first, count).iter().enumerate() {
                    let member = u32::try_from(member).unwrap_or(NONE);
                    let built_in = self
                        .decoration_value_of(struct_id, member, Decoration::BUILT_IN)
                        .ok_or(Rule::BuiltIn)?;
                    self.built_in(stage, class, BuiltIn(built_in), member_ty)?;
                    claim(&mut built_ins, (class.0, built_in))?;
                }
                continue;
            }
            let occupancy = sides.get_mut(side).ok_or(Rule::Interface)?;
            self.user_variable(stage, class, id, pointee, occupancy)?;
        }
        let [inputs, outputs] = sides;
        Ok((inputs.into_varyings(), outputs.into_varyings()))
    }

    /// A built-in of the Vulkan environment's table: the stage and storage
    /// class it may appear in, and its type.
    fn built_in(
        &self,
        stage: Stage,
        class: StorageClass,
        built_in: BuiltIn,
        ty: u32,
    ) -> Result<(), Rule> {
        let input = class == StorageClass::INPUT;
        let output = class == StorageClass::OUTPUT;
        let info = self.ty(ty);
        let float32 = |count: u32| match info.ty {
            Ty::Scalar(Scalar::Float { width: 32 }) => count == 1,
            Ty::Vector {
                scalar: Scalar::Float { width: 32 },
                count: c,
            } => c == count,
            _ => false,
        };
        let int32 = |count: u32| match info.ty {
            Ty::Scalar(Scalar::Int { width: 32, .. }) => count == 1,
            Ty::Vector {
                scalar: Scalar::Int { width: 32, .. },
                count: c,
            } => c == count,
            _ => false,
        };
        let boolean = matches!(info.ty, Ty::Scalar(Scalar::Bool));
        let array_of = |pick: fn(Scalar) -> bool| match info.ty {
            Ty::Array { element, .. } => {
                matches!(self.ty(element).ty, Ty::Scalar(s) if s.width() == 32 && pick(s))
            }
            _ => false,
        };
        let int32_array = array_of(Scalar::is_int);
        let float32_array = array_of(Scalar::is_float);
        let (vertex, fragment, compute) = (
            stage == Stage::Vertex,
            stage == Stage::Fragment,
            stage == Stage::Compute,
        );
        let ok = match built_in {
            BuiltIn::POSITION => vertex && output && float32(4),
            BuiltIn::POINT_SIZE => vertex && output && float32(1),
            BuiltIn::CLIP_DISTANCE | BuiltIn::CULL_DISTANCE => vertex && output && float32_array,
            BuiltIn::VERTEX_INDEX | BuiltIn::INSTANCE_INDEX => vertex && input && int32(1),
            BuiltIn::FRAG_COORD => fragment && input && float32(4),
            BuiltIn::POINT_COORD => fragment && input && float32(2),
            BuiltIn::FRONT_FACING | BuiltIn::HELPER_INVOCATION => fragment && input && boolean,
            BuiltIn::SAMPLE_MASK => fragment && (input || output) && int32_array,
            BuiltIn::FRAG_DEPTH => fragment && output && float32(1),
            BuiltIn::NUM_WORKGROUPS
            | BuiltIn::WORKGROUP_ID
            | BuiltIn::LOCAL_INVOCATION_ID
            | BuiltIn::GLOBAL_INVOCATION_ID => compute && input && int32(3),
            BuiltIn::LOCAL_INVOCATION_INDEX | BuiltIn::NUM_SUBGROUPS | BuiltIn::SUBGROUP_ID => {
                compute && input && int32(1)
            }
            BuiltIn::SUBGROUP_SIZE | BuiltIn::SUBGROUP_LOCAL_INVOCATION_ID => input && int32(1),
            BuiltIn::SUBGROUP_EQ_MASK
            | BuiltIn::SUBGROUP_GE_MASK
            | BuiltIn::SUBGROUP_GT_MASK
            | BuiltIn::SUBGROUP_LE_MASK
            | BuiltIn::SUBGROUP_LT_MASK => input && int32(4),
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(Rule::BuiltIn)
        }
    }

    /// A user-defined input or output: numeric, located, its components and
    /// interpolation as its stage and side allow; its slots taken.
    fn user_variable(
        &self,
        stage: Stage,
        class: StorageClass,
        id: Id,
        pointee: u32,
        slots: &mut Occupancy,
    ) -> Result<(), Rule> {
        let output = class == StorageClass::OUTPUT;
        if self.ty(pointee).holds.contains(Holds::BOOL) {
            return Err(Rule::Location);
        }
        let interpolation = self.interpolation(id, NONE);
        if interpolation != 0
            && ((stage == Stage::Fragment && output) || (stage == Stage::Vertex && !output))
        {
            return Err(Rule::DecorationTarget(Decoration::FLAT));
        }
        // Dual-source blending is the one use of a blend index.
        let index = self
            .decoration_value_of(id, NONE, Decoration::INDEX)
            .unwrap_or(0);
        if self.decorated(id, Decoration::INDEX)
            && (!(stage == Stage::Fragment && output) || index > 1)
        {
            return Err(Rule::DecorationTarget(Decoration::INDEX));
        }
        let location = self.decoration_value_of(id, NONE, Decoration::LOCATION);
        let component = self
            .decoration_value_of(id, NONE, Decoration::COMPONENT)
            .unwrap_or(0);
        let fragment_input = stage == Stage::Fragment && !output;
        let structure = match self.ty(pointee).ty {
            Ty::Struct { first, count } => Some((first, count)),
            _ => None,
        };
        match (location, structure) {
            (Some(location), _) => {
                if let Some((_, count)) = structure {
                    let struct_id = self.ty(pointee).id.ok_or(Rule::Location)?;
                    for member in 0..count {
                        if self
                            .decoration_of(struct_id, member, Decoration::LOCATION)
                            .is_some()
                        {
                            return Err(Rule::Location);
                        }
                    }
                }
                let at = Place {
                    location,
                    component,
                    index,
                    interpolation,
                    fragment_input,
                };
                self.consume(
                    pointee,
                    at,
                    slots,
                    self.decorated(id, Decoration::COMPONENT),
                )?;
            }
            (None, Some((first, count))) => {
                // A block without a location gives each member its own.
                let struct_id = self.ty(pointee).id.ok_or(Rule::Location)?;
                if !self.decorated(struct_id, Decoration::BLOCK) {
                    return Err(Rule::Location);
                }
                for (member, &member_ty) in self.list(first, count).iter().enumerate() {
                    let member = u32::try_from(member).unwrap_or(NONE);
                    let location = self
                        .decoration_value_of(struct_id, member, Decoration::LOCATION)
                        .ok_or(Rule::Location)?;
                    let component =
                        self.decoration_value_of(struct_id, member, Decoration::COMPONENT);
                    let member_interpolation =
                        interpolation | self.interpolation(struct_id, member);
                    if member_interpolation != 0
                        && ((stage == Stage::Fragment && output)
                            || (stage == Stage::Vertex && !output))
                    {
                        return Err(Rule::DecorationTarget(Decoration::FLAT));
                    }
                    let at = Place {
                        location,
                        component: component.unwrap_or(0),
                        index,
                        interpolation: member_interpolation,
                        fragment_input,
                    };
                    self.consume(member_ty, at, slots, component.is_some())?;
                }
            }
            (None, None) => return Err(Rule::Location),
        }
        Ok(())
    }

    /// The interpolation decorations on `id` or its `member`.
    fn interpolation(&self, id: Id, member: u32) -> u8 {
        let mut bits = 0;
        for (decoration, bit) in [
            (Decoration::FLAT, FLAT),
            (Decoration::NO_PERSPECTIVE, NO_PERSPECTIVE),
            (Decoration::CENTROID, CENTROID),
        ] {
            if self.decoration_of(id, member, decoration).is_some() {
                bits |= bit;
            }
        }
        bits
    }

    /// Take the slots a value of type `ty` placed at `at` occupies.
    fn consume(
        &self,
        ty: u32,
        at: Place,
        slots: &mut Occupancy,
        explicit_component: bool,
    ) -> Result<u32, Rule> {
        let info = *self.ty(ty);
        match info.ty {
            Ty::Scalar(scalar) | Ty::Vector { scalar, .. } => {
                let count = match info.ty {
                    Ty::Vector { count, .. } => count,
                    _ => 1,
                };
                if at.fragment_input
                    && (scalar.is_int() || scalar.width() == 64)
                    && at.interpolation & FLAT == 0
                {
                    return Err(Rule::DecorationTarget(Decoration::FLAT));
                }
                let wide = scalar.width() == 64;
                let components = if wide { 2 * count } else { count };
                if explicit_component {
                    let fits = if wide {
                        count <= 2
                            && at.component.is_multiple_of(2)
                            && at.component + components <= 4
                    } else {
                        at.component + components <= 4
                    };
                    if !fits {
                        return Err(Rule::Location);
                    }
                }
                let kind = match scalar {
                    Scalar::Float { .. } => Component::Float,
                    Scalar::Int { signed: true, .. } => Component::Sint,
                    Scalar::Int { signed: false, .. } => Component::Uint,
                    Scalar::Bool => return Err(Rule::Location),
                };
                let mut left = components;
                let mut location = at.location;
                let mut component = at.component;
                while left > 0 {
                    let here = left.min(4 - component);
                    let mask = u8::try_from(((1u32 << here) - 1) << component).unwrap_or(0);
                    slots.take(Slot {
                        location,
                        mask,
                        index: at.index,
                        interpolation: at.interpolation,
                        varying: Varying {
                            location,
                            component,
                            components: here,
                            scalar: kind,
                            width: scalar.width(),
                            index: at.index,
                        },
                    })?;
                    left -= here;
                    location += 1;
                    component = 0;
                }
                Ok(location - at.location)
            }
            Ty::Matrix {
                column, columns, ..
            } => {
                if explicit_component {
                    return Err(Rule::Location);
                }
                self.consume_many(column, columns, at, slots, false)
            }
            Ty::Array { element, length } => {
                let bottom = self.ty(self.innermost(element)).ty;
                if explicit_component && !matches!(bottom, Ty::Scalar(_) | Ty::Vector { .. }) {
                    return Err(Rule::Location);
                }
                self.consume_many(element, length, at, slots, explicit_component)
            }
            Ty::Struct { first, count } => {
                // A structure of no members holds no value an interface
                // could carry.
                if explicit_component || count == 0 {
                    return Err(Rule::Location);
                }
                let mut used = 0u32;
                for &member in self.list(first, count) {
                    let place = Place {
                        location: at.location.checked_add(used).ok_or(Rule::Location)?,
                        component: 0,
                        ..at
                    };
                    used += self.consume(member, place, slots, false)?;
                }
                Ok(used)
            }
            _ => Err(Rule::Location),
        }
    }

    /// Take `count` consecutive copies of `element` from `at`, each starting
    /// at its own location.
    fn consume_many(
        &self,
        element: u32,
        count: u32,
        at: Place,
        slots: &mut Occupancy,
        explicit_component: bool,
    ) -> Result<u32, Rule> {
        let mut used = 0u32;
        for _ in 0..count {
            let place = Place {
                location: at.location.checked_add(used).ok_or(Rule::Location)?,
                ..at
            };
            let taken = self.consume(element, place, slots, explicit_component)?;
            used = used.checked_add(taken).ok_or(Rule::Location)?;
            if at.location.saturating_add(used) > MAX_LOCATIONS {
                return Err(Rule::Limit(super::Limit::Locations));
            }
        }
        Ok(used)
    }
}

/// Record a built-in, refusing one recorded already. The built-in table
/// admits few, so the list stays short.
fn claim(built_ins: &mut Vec<(u32, u32)>, entry: (u32, u32)) -> Result<(), Rule> {
    if built_ins.contains(&entry) {
        return Err(Rule::BuiltIn);
    }
    push(built_ins, entry).map_err(|r| r.rule)
}

impl Occupancy {
    const fn new(fragment_input: bool) -> Self {
        Self {
            masks: [[0; LOCATIONS]; 2],
            interpolation: [None; LOCATIONS],
            fragment_input,
            varyings: Vec::new(),
        }
    }

    /// Take one slot, refusing a location past the bound, a component of a
    /// location taken under the same blend index, and for fragment inputs a
    /// location taken under other interpolation.
    fn take(&mut self, slot: Slot) -> Result<(), Rule> {
        let at = slot.location as usize;
        let index = usize::from(slot.index != 0);
        let (Some(mask), Some(interpolation)) = (
            self.masks.get_mut(index).and_then(|m| m.get_mut(at)),
            self.interpolation.get_mut(at),
        ) else {
            return Err(Rule::Limit(super::Limit::Locations));
        };
        if *mask & slot.mask != 0 {
            return Err(Rule::Location);
        }
        *mask |= slot.mask;
        if self.fragment_input
            && *interpolation.get_or_insert(slot.interpolation) != slot.interpolation
        {
            return Err(Rule::Location);
        }
        push(&mut self.varyings, slot.varying).map_err(|r| r.rule)
    }

    /// The slots taken, by blend index, location and component.
    fn into_varyings(mut self) -> Vec<Varying> {
        self.varyings
            .sort_unstable_by_key(|v| (v.index, v.location, v.component));
        self.varyings
    }
}

/// Where a value is placed: its first location and component, its blend
/// index, its interpolation, and whether it is a fragment input.
#[derive(Copy, Clone, Debug)]
struct Place {
    location: u32,
    component: u32,
    index: u32,
    interpolation: u8,
    fragment_input: bool,
}
