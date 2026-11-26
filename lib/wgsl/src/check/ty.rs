//! Types written in the source: the predeclared types and the generators
//! that make types from template lists, each held to its parameters'
//! rules.

use core::fmt::Write as _;

use super::{Checker, Denotes, ExprInfo, Stage, NONE};
use crate::diag::{error, Code, Outcome, Span, Stop};
use crate::predeclared::{Generator, Named};
use crate::syntax::{ExprId, ExprKind, Run};
use crate::types::{Access, Count, Dim, Holds, Scalar, Space, Texture, Ty, TyId};

/// The deepest a composite type nests: the specification's minimum limit.
pub(crate) const MAX_TYPE_NESTING: u32 = 15;

impl Checker<'_> {
    /// The information of a type expression denoting `ty`.
    pub(crate) fn type_info(ty: TyId) -> ExprInfo {
        ExprInfo {
            ty,
            converted: ty,
            stage: Stage::Const,
            value: NONE,
            denotes: Denotes::Type,
        }
    }

    /// The type a named predeclared type is, `span` naming it.
    pub(crate) fn named(&mut self, named: Named, span: Span) -> Outcome<TyId> {
        let ty = match named {
            Named::Scalar(s) => Ty::Scalar(s),
            Named::Vector(n, s) => Ty::Vector(n, s),
            Named::Matrix(c, r, s) => Ty::Matrix {
                columns: c,
                rows: r,
                scalar: s,
            },
            Named::Sampler { comparison } => Ty::Sampler { comparison },
            Named::External => Ty::Texture(Texture::External),
            Named::Depth(dim) => Ty::Texture(Texture::Depth { dim }),
            Named::DepthMultisampled => Ty::Texture(Texture::DepthMultisampled),
        };
        let id = self.intern(ty)?;
        self.held_to_features(id, span)?;
        Ok(id)
    }

    /// Refuse `ty`, named at `span`, where it holds an `f16` the module has
    /// not enabled.
    pub(crate) fn held_to_features(&self, ty: TyId, span: Span) -> Outcome<()> {
        if !self.features.f16 && self.types.scalar_of(ty) == Some(Scalar::F16) {
            return Err(error(Code::Type, span, |m| {
                m.write_str("`f16` needs `enable f16;`")
            }));
        }
        Ok(())
    }

    /// The type an argument of a template list denotes.
    fn template_type(&self, arg: ExprId) -> Outcome<TyId> {
        let info = self.info(arg);
        if info.denotes == Denotes::Type {
            Ok(info.ty)
        } else {
            Err(error(Code::Misused, self.span_of(arg), |m| {
                m.write_str("a type is needed here")
            }))
        }
    }

    /// The template list's arguments, refused unless there are between
    /// `least` and `most`.
    fn arguments(&self, run: Run, (least, most): (usize, usize), span: Span) -> Outcome<&[ExprId]> {
        let args = self.tree.expr_list(run);
        if args.len() < least || args.len() > most {
            return Err(error(Code::Type, span, |m| {
                if least == most {
                    write!(m, "this type takes {least} template arguments")
                } else {
                    write!(m, "this type takes {least} to {most} template arguments")
                }
            }));
        }
        Ok(args)
    }

    /// The type generator `generator` makes from the template list `run`,
    /// its arguments already resolved; `span` the type written.
    pub(crate) fn generate(&mut self, generator: Generator, run: Run, span: Span) -> Outcome<TyId> {
        match generator {
            Generator::Vector(n) => {
                let args = self.arguments(run, (1, 1), span)?;
                let element = self.template_type(args[0])?;
                let Ty::Scalar(scalar) = self.types.get(element) else {
                    return Err(error(Code::Type, span, |m| {
                        m.write_str("a vector's components are scalars")
                    }));
                };
                self.intern(Ty::Vector(n, scalar))
            }
            Generator::Matrix(columns, rows) => {
                let args = self.arguments(run, (1, 1), span)?;
                let element = self.template_type(args[0])?;
                match self.types.get(element) {
                    Ty::Scalar(scalar @ (Scalar::F32 | Scalar::F16)) => self.intern(Ty::Matrix {
                        columns,
                        rows,
                        scalar,
                    }),
                    _ => Err(error(Code::Type, span, |m| {
                        m.write_str("a matrix's components are f32 or f16")
                    })),
                }
            }
            Generator::Atomic => {
                let args = self.arguments(run, (1, 1), span)?;
                let element = self.template_type(args[0])?;
                match self.types.get(element) {
                    Ty::Scalar(scalar @ (Scalar::I32 | Scalar::U32)) => {
                        self.intern(Ty::Atomic(scalar))
                    }
                    _ => Err(error(Code::Type, span, |m| {
                        m.write_str("an atomic holds an i32 or a u32")
                    })),
                }
            }
            Generator::Array => self.array(run, span),
            Generator::Pointer => self.pointer(run, span),
            Generator::Sampled(dim) => {
                let args = self.arguments(run, (1, 1), span)?;
                let texel = self.texel(args[0], span)?;
                self.intern(Ty::Texture(Texture::Sampled { dim, texel }))
            }
            Generator::Multisampled => {
                let args = self.arguments(run, (1, 1), span)?;
                let texel = self.texel(args[0], span)?;
                self.intern(Ty::Texture(Texture::Multisampled { texel }))
            }
            Generator::Storage(dim) => self.storage_texture(dim, run, span),
        }
    }

    /// A sampled texture's texel type: `f32`, `i32` or `u32`.
    fn texel(&self, arg: ExprId, span: Span) -> Outcome<Scalar> {
        let ty = self.template_type(arg)?;
        match self.types.get(ty) {
            Ty::Scalar(scalar @ (Scalar::F32 | Scalar::I32 | Scalar::U32)) => Ok(scalar),
            _ => Err(error(Code::Type, span, |m| {
                m.write_str("a texture's texels are f32, i32 or u32")
            })),
        }
    }

    fn storage_texture(&mut self, dim: Dim, run: Run, span: Span) -> Outcome<TyId> {
        let args = self.arguments(run, (2, 2), span)?;
        let (format_arg, access_arg) = (args[0], args[1]);
        let Denotes::Format(format) = self.info(format_arg).denotes else {
            return Err(error(Code::Type, self.span_of(format_arg), |m| {
                m.write_str("a storage texture's first argument is a texel format")
            }));
        };
        let Denotes::Access(access) = self.info(access_arg).denotes else {
            return Err(error(Code::Type, self.span_of(access_arg), |m| {
                m.write_str("a storage texture's second argument is an access mode")
            }));
        };
        self.intern(Ty::Texture(Texture::Storage {
            dim,
            format,
            access,
        }))
    }

    /// `ptr<space, store[, access]>`: the access mode written only for
    /// storage, and the pointer one a variable of the space could have.
    fn pointer(&mut self, run: Run, span: Span) -> Outcome<TyId> {
        let args = self.arguments(run, (2, 3), span)?;
        let Denotes::Space(space) = self.info(args[0]).denotes else {
            return Err(error(Code::Type, self.span_of(args[0]), |m| {
                m.write_str("a pointer's first argument is an address space")
            }));
        };
        let store = self.template_type(args[1])?;
        let access = match args.get(2) {
            Some(&arg) => {
                let Denotes::Access(access) = self.info(arg).denotes else {
                    return Err(error(Code::Type, self.span_of(arg), |m| {
                        m.write_str("a pointer's third argument is an access mode")
                    }));
                };
                if space != Space::Storage {
                    return Err(error(Code::AddressSpace, self.span_of(arg), |m| {
                        write!(
                            m,
                            "only a storage pointer names its access mode, not a {} one",
                            space.name()
                        )
                    }));
                }
                if access == Access::Write {
                    return Err(error(Code::AddressSpace, self.span_of(arg), |m| {
                        m.write_str("storage is read or read_write, never write alone")
                    }));
                }
                access
            }
            None => space.default_access(),
        };
        if space == Space::Handle || !self.types.is_storable(store) {
            return Err(error(Code::Type, span, |m| {
                m.write_str("a pointer's store type is storable memory")
            }));
        }
        self.held_to_space(store, space, span)?;
        self.intern(Ty::Pointer {
            space,
            store,
            access,
        })
    }

    /// `array<E[, N]>`: an element of creation-fixed footprint, and a
    /// positive count of a concrete integer type, const or override.
    fn array(&mut self, run: Run, span: Span) -> Outcome<TyId> {
        let args = self.arguments(run, (1, 2), span)?;
        let element = self.template_type(args[0])?;
        let plain = matches!(
            self.types.get(element),
            Ty::Scalar(_)
                | Ty::Vector(..)
                | Ty::Matrix { .. }
                | Ty::Atomic(_)
                | Ty::Array { .. }
                | Ty::Struct(_)
        );
        if !plain || !self.types.has_creation_fixed_footprint(element) {
            return Err(error(Code::Type, span, |m| {
                m.write_str("an array's element is plain and of fixed size")
            }));
        }
        if self.types.depth(element) + 1 > MAX_TYPE_NESTING {
            return Err(error(Code::NestingTooDeep, span, |m| {
                write!(m, "composite types nest more than {MAX_TYPE_NESTING} deep")
            }));
        }
        let count = match args.get(1) {
            None => Count::Runtime,
            Some(&arg) => self.count(arg)?,
        };
        self.intern(Ty::Array { element, count })
    }

    /// An array's element count: a const- or override-expression of a
    /// concrete integer type, positive — a constant's checked now, an
    /// override-expression's once overrides take their values. An
    /// override's bare name identifies the type; any other
    /// override-expression makes a type of its own.
    fn count(&mut self, arg: ExprId) -> Outcome<Count> {
        let span = self.span_of(arg);
        let info = self.info(arg);
        if !info.denotes.is_value() || info.stage == Stage::Runtime {
            return Err(error(Code::Type, span, |m| {
                m.write_str("an array's element count is a const- or override-expression")
            }));
        }
        let ty = self.value_of(arg)?;
        let ty = match self.types.get(ty) {
            Ty::Scalar(Scalar::AbstractInt) => self.types.scalar(Scalar::I32)?,
            Ty::Scalar(Scalar::I32 | Scalar::U32) => ty,
            _ => {
                return Err(error(Code::Type, span, |m| {
                    m.write_str("an array's element count is an integer")
                }))
            }
        };
        self.convert_to(arg, ty)?;
        if info.stage == Stage::Override {
            let count = match info.denotes {
                Denotes::Override(decl) if self.is_bare_name(arg) => Count::Override(decl),
                _ => Count::Expression(arg.0),
            };
            self.types.spell_count(count, span)?;
            return Ok(count);
        }
        self.force(arg)?;
        Ok(Count::Fixed(self.element_count(arg)?))
    }

    /// The evaluated element count `arg`, refused unless positive.
    pub(crate) fn element_count(&self, arg: ExprId) -> Outcome<u32> {
        let value = self.integer_value(arg)?;
        if value <= 0 || value > i128::from(u32::MAX) {
            return Err(error(Code::Type, self.span_of(arg), |m| {
                write!(m, "an array's element count is positive, not {value}")
            }));
        }
        u32::try_from(value).map_err(|_| Stop::Internal)
    }

    /// Whether expression `id` is a name alone.
    fn is_bare_name(&self, id: ExprId) -> bool {
        matches!(
            self.tree.expr(id).map(|e| e.kind),
            Some(ExprKind::Ident(ident)) if ident.template.is_empty()
        )
    }

    /// Refuse a store type `store` its address space `space` does not
    /// hold, at `span`: an atomic only where it may be modified, a
    /// runtime-sized array only in storage, an override-sized array only in
    /// workgroup memory.
    pub(crate) fn held_to_space(&self, store: TyId, space: Space, span: Span) -> Outcome<()> {
        let refuse = |what: &str| {
            Err(error(Code::AddressSpace, span, |m| {
                write!(
                    m,
                    "{what} cannot be held in the {} address space",
                    space.name()
                )
            }))
        };
        let holds = |kind| self.types.holds(store, kind);
        if holds(Holds::ATOMIC) && !matches!(space, Space::Storage | Space::Workgroup) {
            return refuse("an atomic");
        }
        if holds(Holds::RUNTIME_ARRAY) && space != Space::Storage {
            return refuse("a runtime-sized array");
        }
        if holds(Holds::OVERRIDE_ARRAY) && space != Space::Workgroup {
            return refuse("an override-sized array");
        }
        Ok(())
    }
}
