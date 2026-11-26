//! The builder: a module written from Rust and handed back only once the
//! validator admits it (`plans/SHADER.md` SH3).
//!
//! A [`Builder`] gathers each of a module's sections as it is written —
//! capabilities, extensions, entry points, execution modes, decorations, the
//! types, constants and variables its functions share, and the functions —
//! and [`Builder::finish`] lays them out in the order SPIR-V requires, decodes
//! the words and validates them. A builder bug is therefore a refusal, never a
//! module that runs.
//!
//! Types and constants are interned: asking for one twice answers the same
//! id, since SPIR-V refuses two non-aggregate types alike. Arrays and
//! structures are aggregates, so each is a type of its own, for whoever made
//! it to decorate.
//!
//! A [`Function`] is written a block at a time. Its structured constructs
//! open, merge and close their own blocks, and its `Function`-class variables
//! are laid at the head of its first block wherever they were asked for.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_collections::OrderedMap;
use tairix_util::fallible;

use crate::encode::{encode, Operands, Trail};
use crate::enumerant::{
    AddressingModel, Capability, Decoration, ExecutionMode, ExecutionModel, FunctionControl,
    LoopControl, MemoryModel, SelectionControl, StorageClass,
};
use crate::{
    Admitted, Arg, DecodeError, EncodeError, Encoder, GlslStd450, Id, ImageType, Module, Op,
    Refusal, Version, MAX_INSTRUCTIONS, MAX_MODULE_BYTES,
};

/// Why a module could not be built.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BuildError {
    /// The encoder refused an instruction.
    Encode(EncodeError),
    /// The words written are not a module the decoder takes.
    Decode(DecodeError),
    /// The validator refused the module.
    Refused(Refusal),
    /// An instruction was given with no block open to hold it: after a
    /// block's terminator, before the next block began.
    NoBlock,
    /// A block was begun, or a function ended, while a block was still open.
    Unterminated,
    /// The memory a section needed could not be had.
    OutOfMemory,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(f, "encoding: {error}"),
            Self::Decode(error) => write!(f, "decoding: {error:?}"),
            Self::Refused(refusal) => {
                write!(f, "refused at word {}: {:?}", refusal.word, refusal.rule)
            }
            Self::NoBlock => f.write_str("an instruction outside any block"),
            Self::Unterminated => f.write_str("a block left without its terminator"),
            Self::OutOfMemory => f.write_str("out of memory"),
        }
    }
}

impl From<EncodeError> for BuildError {
    fn from(error: EncodeError) -> Self {
        Self::Encode(error)
    }
}

/// Where an instruction is written: a section of the module, held as its
/// encoded bytes until the module is laid out.
#[derive(Copy, Clone, Debug)]
enum Section {
    Entries,
    Modes,
    Annotations,
    Globals,
}

/// Encode `op` over `operands` onto `section`, counting its bytes against
/// the module's.
fn put(
    section: &mut Vec<u8>,
    written: &mut usize,
    bound: u32,
    op: Op,
    operands: Operands<'_>,
) -> Result<(), BuildError> {
    let before = section.len();
    let limit = before.saturating_add(MAX_MODULE_BYTES.saturating_sub(*written));
    encode(section, op, operands, bound, limit)?;
    *written += section.len() - before;
    Ok(())
}

/// The bytes of a block's label: an `OpLabel`, two words.
const LABEL_BYTES: usize = 8;

/// An interned type or constant: its opcode and every word after its result.
type Key = (u16, Vec<u32>);

/// The words `args` occupy, as an interning key holds them.
fn key_words(lead: Option<Id>, args: &[Arg<'_>]) -> Option<Vec<u32>> {
    let mut words = Vec::new();
    if !fallible::reserve(&mut words, args.len() * 2 + 1) {
        return None;
    }
    words.extend(lead.map(Id::get));
    for arg in args {
        match *arg {
            Arg::Id(id) => words.push(id.get()),
            Arg::Word(word) => words.push(word),
            Arg::Wide(wide) => {
                let bytes = wide.to_le_bytes();
                words.extend(
                    bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|half| u32::from_le_bytes(*half)),
                );
            }
            // No type or constant interned takes a string.
            Arg::Text(_) => return None,
        }
    }
    Some(words)
}

/// A module being written.
#[derive(Debug)]
pub struct Builder {
    encoder: Encoder,
    capabilities: Vec<Capability>,
    extensions: Vec<String>,
    glsl: Option<Id>,
    entries: Vec<u8>,
    modes: Vec<u8>,
    annotations: Vec<u8>,
    globals: Vec<u8>,
    functions: Vec<u8>,
    /// The bytes every section holds, together.
    written: usize,
    /// Each interned type and constant and its id.
    interned: OrderedMap<Key, Id>,
}

impl Builder {
    /// A module of `version`, carrying `generator` in its header.
    ///
    /// # Errors
    ///
    /// [`BuildError::Encode`] when the header cannot be had.
    pub fn new(version: Version, generator: u32) -> Result<Self, BuildError> {
        Ok(Self {
            encoder: Encoder::new(version, generator)?,
            capabilities: Vec::new(),
            extensions: Vec::new(),
            glsl: None,
            entries: Vec::new(),
            modes: Vec::new(),
            annotations: Vec::new(),
            globals: Vec::new(),
            functions: Vec::new(),
            written: 0,
            interned: OrderedMap::new(),
        })
    }

    /// A fresh id: for a block, or a function referred to before it is
    /// defined.
    ///
    /// # Errors
    ///
    /// [`BuildError::Encode`] once every id has been issued.
    pub fn id(&mut self) -> Result<Id, BuildError> {
        Ok(self.encoder.next_id()?)
    }

    /// Declare `capability`, once however often it is asked for.
    ///
    /// # Errors
    ///
    /// [`BuildError::OutOfMemory`].
    pub fn capability(&mut self, capability: Capability) -> Result<(), BuildError> {
        if self.capabilities.contains(&capability) {
            return Ok(());
        }
        fallible::try_push(&mut self.capabilities, capability).map_err(|_| BuildError::OutOfMemory)
    }

    /// Declare the extension `name`, once however often it is asked for.
    ///
    /// # Errors
    ///
    /// [`BuildError::OutOfMemory`].
    pub fn extension(&mut self, name: &str) -> Result<(), BuildError> {
        if self.extensions.iter().any(|held| held == name) {
            return Ok(());
        }
        let name = fallible::concatenated(&[name]).ok_or(BuildError::OutOfMemory)?;
        fallible::try_push(&mut self.extensions, name).map_err(|_| BuildError::OutOfMemory)
    }

    /// The id the `GLSL.std.450` set is imported as, imported once.
    ///
    /// # Errors
    ///
    /// [`BuildError::Encode`] once every id has been issued.
    pub fn glsl(&mut self) -> Result<Id, BuildError> {
        if let Some(id) = self.glsl {
            return Ok(id);
        }
        let id = self.id()?;
        self.glsl = Some(id);
        Ok(id)
    }

    /// Declare `function` an entry point of `model` named `name`, over the
    /// module-scope variables its `interface` names.
    ///
    /// # Errors
    ///
    /// [`BuildError::OutOfMemory`].
    pub fn entry_point(
        &mut self,
        model: ExecutionModel,
        function: Id,
        name: &str,
        interface: &[Id],
    ) -> Result<(), BuildError> {
        self.write(
            Section::Entries,
            Op::ENTRY_POINT,
            (
                &[],
                &[Arg::Word(model.0), Arg::Id(function), Arg::Text(name)],
                Trail::Ids(interface),
            ),
        )
    }

    /// Set `mode`, with its `literals`, on the entry point `function`.
    ///
    /// # Errors
    ///
    /// [`BuildError::OutOfMemory`].
    pub fn execution_mode(
        &mut self,
        function: Id,
        mode: ExecutionMode,
        literals: &[u32],
    ) -> Result<(), BuildError> {
        self.write(
            Section::Modes,
            Op::EXECUTION_MODE,
            (&[function], &[Arg::Word(mode.0)], Trail::Words(literals)),
        )
    }

    /// Decorate `target` with `decoration` and its `literals`.
    ///
    /// # Errors
    ///
    /// [`BuildError::OutOfMemory`].
    pub fn decorate(
        &mut self,
        target: Id,
        decoration: Decoration,
        literals: &[u32],
    ) -> Result<(), BuildError> {
        self.write(
            Section::Annotations,
            Op::DECORATE,
            (
                &[target],
                &[Arg::Word(decoration.0)],
                Trail::Words(literals),
            ),
        )
    }

    /// Decorate member `member` of `structure` with `decoration` and its
    /// `literals`.
    ///
    /// # Errors
    ///
    /// [`BuildError::OutOfMemory`].
    pub fn member_decorate(
        &mut self,
        structure: Id,
        member: u32,
        decoration: Decoration,
        literals: &[u32],
    ) -> Result<(), BuildError> {
        self.write(
            Section::Annotations,
            Op::MEMBER_DECORATE,
            (
                &[structure],
                &[Arg::Word(member), Arg::Word(decoration.0)],
                Trail::Words(literals),
            ),
        )
    }

    /// Write `op` over `operands` to `section`.
    fn write(
        &mut self,
        section: Section,
        op: Op,
        operands: Operands<'_>,
    ) -> Result<(), BuildError> {
        let bound = self.encoder.bound();
        let target = match section {
            Section::Entries => &mut self.entries,
            Section::Modes => &mut self.modes,
            Section::Annotations => &mut self.annotations,
            Section::Globals => &mut self.globals,
        };
        put(target, &mut self.written, bound, op, operands)
    }

    /// The bytes of the instructions written so far, the module's header and
    /// its capabilities, extensions, imports and memory model aside.
    #[must_use]
    pub const fn written(&self) -> usize {
        self.written
    }

    /// The type or constant `op` declares with `args`, interned: the id it
    /// was given when first asked for, or a fresh one.
    fn intern(
        &mut self,
        op: Op,
        result_type: Option<Id>,
        args: &[Arg<'_>],
    ) -> Result<Id, BuildError> {
        let key = (
            op.code(),
            key_words(result_type, args).ok_or(BuildError::OutOfMemory)?,
        );
        if let Some(&id) = self.interned.get(&key) {
            return Ok(id);
        }
        let id = self.declare(op, result_type, args)?;
        self.interned
            .try_insert(key, id)
            .map_err(|_| BuildError::OutOfMemory)?;
        Ok(id)
    }

    /// Declare a fresh type, constant or variable shared by the module's
    /// functions: `op` defining a new id, its `result_type` first where it
    /// has one, then `args`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Encode`] once every id has been issued, and
    /// [`BuildError::OutOfMemory`].
    pub fn declare(
        &mut self,
        op: Op,
        result_type: Option<Id>,
        args: &[Arg<'_>],
    ) -> Result<Id, BuildError> {
        let id = self.id()?;
        let typed = [result_type.unwrap_or(id), id];
        let lead = if result_type.is_some() {
            &typed[..]
        } else {
            &typed[1..]
        };
        self.write(Section::Globals, op, (lead, args, Trail::None))?;
        Ok(id)
    }

    /// `void`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn void(&mut self) -> Result<Id, BuildError> {
        self.intern(Op::TYPE_VOID, None, &[])
    }

    /// The Boolean type.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn boolean(&mut self) -> Result<Id, BuildError> {
        self.intern(Op::TYPE_BOOL, None, &[])
    }

    /// The integer type `width` bits wide, `signed` or not.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn int(&mut self, width: u32, signed: bool) -> Result<Id, BuildError> {
        self.intern(
            Op::TYPE_INT,
            None,
            &[Arg::Word(width), Arg::Word(u32::from(signed))],
        )
    }

    /// The float type `width` bits wide.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn float(&mut self, width: u32) -> Result<Id, BuildError> {
        self.intern(Op::TYPE_FLOAT, None, &[Arg::Word(width)])
    }

    /// The vector of `count` `component`s.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn vector(&mut self, component: Id, count: u32) -> Result<Id, BuildError> {
        self.intern(
            Op::TYPE_VECTOR,
            None,
            &[Arg::Id(component), Arg::Word(count)],
        )
    }

    /// The matrix of `count` `column`s.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn matrix(&mut self, column: Id, count: u32) -> Result<Id, BuildError> {
        self.intern(Op::TYPE_MATRIX, None, &[Arg::Id(column), Arg::Word(count)])
    }

    /// The sampler type.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn sampler(&mut self) -> Result<Id, BuildError> {
        self.intern(Op::TYPE_SAMPLER, None, &[])
    }

    /// The image type `image` describes.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn image(&mut self, image: &ImageType<Id>) -> Result<Id, BuildError> {
        let sampling = if image.storage { 2 } else { 1 };
        self.intern(
            Op::TYPE_IMAGE,
            None,
            &[
                Arg::Id(image.sampled),
                Arg::Word(image.dim.0),
                Arg::Word(image.depth),
                Arg::Word(u32::from(image.arrayed)),
                Arg::Word(u32::from(image.multisampled)),
                Arg::Word(sampling),
                Arg::Word(image.format.0),
            ],
        )
    }

    /// The sampled image type over `image`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn sampled_image(&mut self, image: Id) -> Result<Id, BuildError> {
        self.intern(Op::TYPE_SAMPLED_IMAGE, None, &[Arg::Id(image)])
    }

    /// The pointer to `pointee` in `class`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn pointer(&mut self, class: StorageClass, pointee: Id) -> Result<Id, BuildError> {
        self.intern(
            Op::TYPE_POINTER,
            None,
            &[Arg::Word(class.0), Arg::Id(pointee)],
        )
    }

    /// The function type answering `result` from `params`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn function_type(&mut self, result: Id, params: &[Id]) -> Result<Id, BuildError> {
        let mut args = Vec::new();
        if !fallible::reserve(&mut args, params.len() + 1) {
            return Err(BuildError::OutOfMemory);
        }
        args.push(Arg::Id(result));
        args.extend(params.iter().map(|&id| Arg::Id(id)));
        self.intern(Op::TYPE_FUNCTION, None, &args)
    }

    /// A fresh array of `length` `element`s, `length` a constant's id.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn array(&mut self, element: Id, length: Id) -> Result<Id, BuildError> {
        self.declare(Op::TYPE_ARRAY, None, &[Arg::Id(element), Arg::Id(length)])
    }

    /// A fresh array of `element`s whose length the binding it is reached
    /// through decides.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn runtime_array(&mut self, element: Id) -> Result<Id, BuildError> {
        self.declare(Op::TYPE_RUNTIME_ARRAY, None, &[Arg::Id(element)])
    }

    /// A fresh structure of `members`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn structure(&mut self, members: &[Id]) -> Result<Id, BuildError> {
        let mut args = Vec::new();
        if !fallible::reserve(&mut args, members.len()) {
            return Err(BuildError::OutOfMemory);
        }
        args.extend(members.iter().map(|&id| Arg::Id(id)));
        self.declare(Op::TYPE_STRUCT, None, &args)
    }

    /// The constant of type `ty` whose bits are `bits`: a 32-bit scalar, or
    /// a narrower one held in the low bits.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn constant(&mut self, ty: Id, bits: u32) -> Result<Id, BuildError> {
        self.intern(Op::CONSTANT, Some(ty), &[Arg::Word(bits)])
    }

    /// The constant of 64-bit type `ty` whose bits are `bits`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn constant_wide(&mut self, ty: Id, bits: u64) -> Result<Id, BuildError> {
        self.intern(Op::CONSTANT, Some(ty), &[Arg::Wide(bits)])
    }

    /// The Boolean constant `value`, of Boolean type `ty`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn constant_bool(&mut self, ty: Id, value: bool) -> Result<Id, BuildError> {
        let op = if value {
            Op::CONSTANT_TRUE
        } else {
            Op::CONSTANT_FALSE
        };
        self.intern(op, Some(ty), &[])
    }

    /// The constant of composite type `ty` holding the constants `parts`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn constant_composite(&mut self, ty: Id, parts: &[Id]) -> Result<Id, BuildError> {
        let mut args = Vec::new();
        if !fallible::reserve(&mut args, parts.len()) {
            return Err(BuildError::OutOfMemory);
        }
        args.extend(parts.iter().map(|&id| Arg::Id(id)));
        self.intern(Op::CONSTANT_COMPOSITE, Some(ty), &args)
    }

    /// The constant of type `ty` with every bit clear.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn constant_null(&mut self, ty: Id) -> Result<Id, BuildError> {
        self.intern(Op::CONSTANT_NULL, Some(ty), &[])
    }

    /// The 32-bit unsigned integer constant `value`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn uint(&mut self, value: u32) -> Result<Id, BuildError> {
        let ty = self.int(32, false)?;
        self.constant(ty, value)
    }

    /// The 32-bit signed integer constant `value`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn sint(&mut self, value: i32) -> Result<Id, BuildError> {
        let ty = self.int(32, true)?;
        self.constant(ty, u32::from_ne_bytes(value.to_ne_bytes()))
    }

    /// The 32-bit float constant `value`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn real(&mut self, value: f32) -> Result<Id, BuildError> {
        let ty = self.float(32)?;
        self.constant(ty, value.to_bits())
    }

    /// A module-scope variable of `pointer` type in `class`.
    ///
    /// # Errors
    ///
    /// As [`Self::declare`].
    pub fn variable(&mut self, pointer: Id, class: StorageClass) -> Result<Id, BuildError> {
        self.declare(Op::VARIABLE, Some(pointer), &[Arg::Word(class.0)])
    }

    /// Define `function`, an id from [`Self::id`], answering `result` from
    /// parameters of the types `params`: `write` writes it, from its first
    /// block on, given the parameters' ids, and must end every block it
    /// opens. Its error is its own, a builder's among what it holds.
    ///
    /// # Errors
    ///
    /// Whatever `write` answered; [`BuildError::Unterminated`] where it left
    /// its last block open; as [`Self::declare`].
    pub fn define<F, E>(
        &mut self,
        function: Id,
        result: Id,
        params: &[Id],
        write: F,
    ) -> Result<(), E>
    where
        F: FnOnce(&mut Function<'_>, &[Id]) -> Result<(), E>,
        E: From<BuildError>,
    {
        let ty = self.function_type(result, params)?;
        let mut head = Vec::new();
        put(
            &mut head,
            &mut self.written,
            self.encoder.bound(),
            Op::FUNCTION,
            (
                &[result, function],
                &[Arg::Word(FunctionControl::NONE.0), Arg::Id(ty)],
                Trail::None,
            ),
        )?;
        let mut values = Vec::new();
        if !fallible::reserve(&mut values, params.len()) {
            return Err(BuildError::OutOfMemory.into());
        }
        for &param in params {
            let value = self.id()?;
            put(
                &mut head,
                &mut self.written,
                self.encoder.bound(),
                Op::FUNCTION_PARAMETER,
                (&[param, value], &[], Trail::None),
            )?;
            values.push(value);
        }
        let entry = self.id()?;
        let mut writing = Function {
            builder: self,
            body: Vec::new(),
            locals: Vec::new(),
            open: None,
        };
        writing.begin(entry)?;
        write(&mut writing, &values)?;
        if writing.open.is_some() {
            return Err(BuildError::Unterminated.into());
        }
        let Function { body, locals, .. } = writing;
        if !fallible::reserve(&mut self.functions, head.len() + locals.len() + body.len()) {
            return Err(BuildError::OutOfMemory.into());
        }
        // The first block's label, then its variables, then the rest.
        let (label, rest) = body.split_at(LABEL_BYTES.min(body.len()));
        for part in [&head[..], label, &locals, rest] {
            self.functions.extend_from_slice(part);
        }
        put(
            &mut self.functions,
            &mut self.written,
            self.encoder.bound(),
            Op::FUNCTION_END,
            (&[], &[], Trail::None),
        )?;
        Ok(())
    }

    /// Lay the module out, decode it, and validate it.
    ///
    /// # Errors
    ///
    /// [`BuildError::Encode`], [`BuildError::Decode`] or
    /// [`BuildError::Refused`] for a module the encoder, the decoder or the
    /// validator refused.
    pub fn finish(self) -> Result<Admitted, BuildError> {
        Admitted::new(self.lay_out()?).map_err(BuildError::Refused)
    }

    /// The module laid out and decoded but not validated: what a test reads
    /// a refusal's word offset against.
    ///
    /// # Errors
    ///
    /// [`BuildError::Encode`] or [`BuildError::Decode`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn lay_out_unvalidated(self) -> Result<Module, BuildError> {
        self.lay_out()
    }

    fn lay_out(self) -> Result<Module, BuildError> {
        let Self {
            mut encoder,
            capabilities,
            extensions,
            glsl,
            entries,
            modes,
            annotations,
            globals,
            functions,
            written,
            ..
        } = self;
        // The preamble is a few dozen bytes beside the sections.
        encoder.room_for(written.saturating_add(256))?;
        for capability in capabilities {
            encoder.emit(Op::CAPABILITY, &[Arg::Word(capability.0)])?;
        }
        for extension in &extensions {
            encoder.emit(Op::EXTENSION, &[Arg::Text(extension)])?;
        }
        if let Some(set) = glsl {
            encoder.emit(
                Op::EXT_INST_IMPORT,
                &[Arg::Id(set), Arg::Text("GLSL.std.450")],
            )?;
        }
        encoder.emit(
            Op::MEMORY_MODEL,
            &[
                Arg::Word(AddressingModel::LOGICAL.0),
                Arg::Word(MemoryModel::GLSL450.0),
            ],
        )?;
        // Each section is let go as soon as it is copied.
        for section in [entries, modes, annotations, globals, functions] {
            encoder.append(&section)?;
        }
        crate::decode::decode_owned(encoder.finish(), MAX_INSTRUCTIONS).map_err(BuildError::Decode)
    }
}

/// Whether `op` ends the block it is written in.
fn terminates(op: Op) -> bool {
    [
        Op::BRANCH,
        Op::BRANCH_CONDITIONAL,
        Op::SWITCH,
        Op::RETURN,
        Op::RETURN_VALUE,
        Op::KILL,
        Op::UNREACHABLE,
        Op::TERMINATE_INVOCATION,
    ]
    .contains(&op)
}

/// A function body being written, a block at a time.
#[derive(Debug)]
pub struct Function<'b> {
    builder: &'b mut Builder,
    body: Vec<u8>,
    /// Its `Function`-class variables, laid at the head of its first block.
    locals: Vec<u8>,
    /// The block open, begun and not yet terminated.
    open: Option<Id>,
}

impl Function<'_> {
    /// The module the function belongs to, for the types and constants its
    /// body names.
    pub fn module(&mut self) -> &mut Builder {
        self.builder
    }

    /// A fresh id for a block the body branches to before beginning it.
    ///
    /// # Errors
    ///
    /// As [`Builder::id`].
    pub fn label(&mut self) -> Result<Id, BuildError> {
        self.builder.id()
    }

    /// Begin the block `label`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Unterminated`] while a block is still open.
    pub fn begin(&mut self, label: Id) -> Result<(), BuildError> {
        if self.open.is_some() {
            return Err(BuildError::Unterminated);
        }
        let bound = self.builder.encoder.bound();
        put(
            &mut self.body,
            &mut self.builder.written,
            bound,
            Op::LABEL,
            (&[label], &[], Trail::None),
        )?;
        self.open = Some(label);
        Ok(())
    }

    /// The label of the block open, if one is: the predecessor an `OpPhi`
    /// names for a value made in it.
    #[must_use]
    pub const fn block(&self) -> Option<Id> {
        self.open
    }

    fn place(&mut self, op: Op, operands: Operands<'_>) -> Result<(), BuildError> {
        let Some(label) = self.open else {
            return Err(BuildError::NoBlock);
        };
        let bound = self.builder.encoder.bound();
        put(
            &mut self.body,
            &mut self.builder.written,
            bound,
            op,
            operands,
        )?;
        self.open = if terminates(op) { None } else { Some(label) };
        Ok(())
    }

    /// Write `op` of `result_type` over the ids `operands`: the value it
    /// defines.
    ///
    /// # Errors
    ///
    /// [`BuildError::NoBlock`] with no block open; as [`Builder::id`].
    pub fn op(&mut self, op: Op, result_type: Id, operands: &[Id]) -> Result<Id, BuildError> {
        let id = self.builder.id()?;
        self.place(op, (&[result_type, id], &[], Trail::Ids(operands)))?;
        Ok(id)
    }

    /// [`Self::op`] over operands that are not all ids.
    ///
    /// # Errors
    ///
    /// As [`Self::op`].
    pub fn op_args(&mut self, op: Op, result_type: Id, args: &[Arg<'_>]) -> Result<Id, BuildError> {
        let id = self.builder.id()?;
        self.place(op, (&[result_type, id], args, Trail::None))?;
        Ok(id)
    }

    /// Write `op`, which defines nothing, over the ids `operands`. A
    /// terminator ends the block.
    ///
    /// # Errors
    ///
    /// [`BuildError::NoBlock`] with no block open.
    pub fn emit(&mut self, op: Op, operands: &[Id]) -> Result<(), BuildError> {
        self.place(op, (operands, &[], Trail::None))
    }

    /// [`Self::emit`] over operands that are not all ids.
    ///
    /// # Errors
    ///
    /// As [`Self::emit`].
    pub fn emit_args(&mut self, op: Op, args: &[Arg<'_>]) -> Result<(), BuildError> {
        self.place(op, (&[], args, Trail::None))
    }

    /// The `GLSL.std.450` instruction `inst` of `result_type` over the ids
    /// `operands`.
    ///
    /// # Errors
    ///
    /// As [`Self::op`].
    pub fn glsl(
        &mut self,
        inst: GlslStd450,
        result_type: Id,
        operands: &[Id],
    ) -> Result<Id, BuildError> {
        let set = self.builder.glsl()?;
        let id = self.builder.id()?;
        self.place(
            Op::EXT_INST,
            (
                &[result_type, id, set],
                &[Arg::Word(inst.0)],
                Trail::Ids(operands),
            ),
        )?;
        Ok(id)
    }

    /// A `Function`-class variable of `pointer` type, laid at the head of the
    /// first block wherever it is asked for.
    ///
    /// # Errors
    ///
    /// As [`Builder::id`].
    pub fn local(&mut self, pointer: Id) -> Result<Id, BuildError> {
        let id = self.builder.id()?;
        let bound = self.builder.encoder.bound();
        put(
            &mut self.locals,
            &mut self.builder.written,
            bound,
            Op::VARIABLE,
            (
                &[pointer, id],
                &[Arg::Word(StorageClass::FUNCTION.0)],
                Trail::None,
            ),
        )?;
        Ok(id)
    }

    /// Branch to `target` if a block is still open.
    fn fall_to(&mut self, target: Id) -> Result<(), BuildError> {
        if self.open.is_some() {
            self.emit(Op::BRANCH, &[target])?;
        }
        Ok(())
    }

    /// Run `then` where `condition` holds, then go on.
    ///
    /// # Errors
    ///
    /// Whatever `then` answered; as [`Self::op`].
    pub fn if_then<T>(&mut self, condition: Id, then: T) -> Result<(), BuildError>
    where
        T: FnOnce(&mut Self) -> Result<(), BuildError>,
    {
        let (taken, merge) = (self.label()?, self.label()?);
        self.select(merge)?;
        self.emit(Op::BRANCH_CONDITIONAL, &[condition, taken, merge])?;
        self.begin(taken)?;
        then(self)?;
        self.fall_to(merge)?;
        self.begin(merge)
    }

    /// Run `then` where `condition` holds and `otherwise` where it does not,
    /// then go on.
    ///
    /// # Errors
    ///
    /// Whatever either answered; as [`Self::op`].
    pub fn if_else<T, O>(&mut self, condition: Id, then: T, otherwise: O) -> Result<(), BuildError>
    where
        T: FnOnce(&mut Self) -> Result<(), BuildError>,
        O: FnOnce(&mut Self) -> Result<(), BuildError>,
    {
        let (taken, other, merge) = (self.label()?, self.label()?, self.label()?);
        self.select(merge)?;
        self.emit(Op::BRANCH_CONDITIONAL, &[condition, taken, other])?;
        self.begin(taken)?;
        then(self)?;
        self.fall_to(merge)?;
        self.begin(other)?;
        otherwise(self)?;
        self.fall_to(merge)?;
        self.begin(merge)
    }

    fn select(&mut self, merge: Id) -> Result<(), BuildError> {
        self.emit_args(
            Op::SELECTION_MERGE,
            &[Arg::Id(merge), Arg::Word(SelectionControl::NONE.0)],
        )
    }

    /// Loop while the Boolean `condition` answers holds: each time round,
    /// `body`, then `step`, the loop's continue construct.
    ///
    /// # Errors
    ///
    /// Whatever `condition`, `body` or `step` answered; as [`Self::op`].
    pub fn while_loop<C, B, S>(&mut self, condition: C, body: B, step: S) -> Result<(), BuildError>
    where
        C: FnOnce(&mut Self) -> Result<Id, BuildError>,
        B: FnOnce(&mut Self) -> Result<(), BuildError>,
        S: FnOnce(&mut Self) -> Result<(), BuildError>,
    {
        let header = self.label()?;
        let (test, inside, next, merge) =
            (self.label()?, self.label()?, self.label()?, self.label()?);
        self.emit(Op::BRANCH, &[header])?;
        self.begin(header)?;
        self.emit_args(
            Op::LOOP_MERGE,
            &[
                Arg::Id(merge),
                Arg::Id(next),
                Arg::Word(LoopControl::NONE.0),
            ],
        )?;
        self.emit(Op::BRANCH, &[test])?;
        self.begin(test)?;
        let holds = condition(self)?;
        self.emit(Op::BRANCH_CONDITIONAL, &[holds, inside, merge])?;
        self.begin(inside)?;
        body(self)?;
        self.fall_to(next)?;
        self.begin(next)?;
        step(self)?;
        self.fall_to(header)?;
        self.begin(merge)
    }
}

#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;
