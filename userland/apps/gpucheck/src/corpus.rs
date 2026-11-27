//! The differential corpus (`plans/SHADER.md` SH7): every instruction the
//! validator admits in a body, written with the builder into kernels each
//! device runs beside the software device, every word a device leaves held
//! to the software device's within what Vulkan states the instruction that
//! computed it to: bit for bit, or within its units in the last place or its
//! absolute error. A kernel's inputs are words of its buffer, never
//! constants, so a device computes each instruction rather than folding it
//! as it compiles. The image instructions run in the image work's
//! dispatches; those only a fragment stage runs, in a draw (`fragment`).
//!
//! Every value is one whose result every conforming device defines alike:
//! no NaN, infinity or subnormal, no integer past what its instruction
//! defines, every index in range, one invocation where order would show. A
//! device that does not state what a kernel needs — its capabilities, or the
//! formats and features it uses — is not held to it.

mod fragment;
mod images;
mod numbers;
mod structure;

use alloc::vec::Vec;

use tairix_gpu::{Buffer, BufferUsage, Context, DescriptorType, Device, Error, LayoutBinding};
use tairix_spirv::enumerant::{
    BuiltIn, Capability, Decoration, ExecutionMode, ExecutionModel, StorageClass,
};
use tairix_spirv::{Admitted, Arg, BuildError, Builder, Function, Id, Op, Stage, Stages, Version};

use crate::shader::{read_words, run_over, Kernel, Outcome};
use crate::{Difference, Refusal};

/// The step a refusal to make the corpus names.
const CORPUS: &str = "the corpus";

/// The version a corpus kernel is written in unless its family says
/// otherwise: the first with the non-uniform group instructions.
const VERSION: Version = Version { major: 1, minor: 3 };

/// The first version whose entry points list every global they use.
const LISTS_EVERY_GLOBAL: Version = Version { major: 1, minor: 4 };

/// A word's place in a kernel's buffer.
type At = u32;

/// How a device's word is held to the software device's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Held {
    /// Bit for bit.
    Exact,
    /// As singles, within this many units in the last place.
    Ulps(u32),
    /// As singles, within 2 to the minus this of each other.
    Within(u8),
}

impl Held {
    /// Whether `device` is held to `reference`.
    #[must_use]
    pub fn holds(self, device: u32, reference: u32) -> bool {
        match self {
            Self::Exact => device == reference,
            Self::Ulps(units) => ordered(device).abs_diff(ordered(reference)) <= u64::from(units),
            Self::Within(power) => {
                let apart = f32::from_bits(device) - f32::from_bits(reference);
                let bound = f32::from_bits(u32::from(127u8.saturating_sub(power)) << 23);
                f32::from_bits(apart.to_bits() & !(1 << 31)) <= bound
            }
        }
    }
}

/// A single's bits as a count that runs on across zero, so neighbouring
/// singles are neighbouring counts.
fn ordered(bits: u32) -> i64 {
    let magnitude = i64::from(bits & !(1 << 31));
    if bits >> 31 == 1 {
        -magnitude
    } else {
        magnitude
    }
}

/// One word a kernel writes: the instruction it is the answer of, and how it
/// is held.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Output {
    /// The instruction, as the specification names it.
    pub case: &'static str,
    /// How a device's word is held to the software device's.
    pub held: Held,
}

/// What a device answered one kernel of the corpus.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ran {
    /// The kernel's family.
    pub family: &'static str,
    /// The capabilities its kernel declares past `Shader`.
    pub capabilities: &'static [Capability],
    /// The words it writes, by place, and how each is held.
    pub outputs: Vec<(u32, Output)>,
    /// The words it left; `None` where the device does not state what it
    /// needs: its capabilities, or the formats and features it uses.
    pub left: Option<Vec<u32>>,
}

/// The corpus's families, in the order each device runs them.
fn families() -> impl Iterator<Item = &'static Family> {
    numbers::FAMILIES
        .iter()
        .chain(structure::FAMILIES)
        .chain([&images::FAMILY, &fragment::FAMILY])
}

/// One kernel of the corpus: how it runs, the bytes its buffer starts as,
/// and the words it writes.
#[derive(Debug)]
struct Entry {
    run: Run,
    start: Vec<u8>,
    outputs: Vec<(At, Output)>,
}

/// How a kernel of the corpus runs.
#[derive(Debug)]
enum Run {
    /// Over its buffer alone, as `gpucheck --shader` runs a kernel.
    Kernel(Kernel),
    /// Binding the image work's images beside its buffer.
    Images(Admitted),
    /// As a draw's fragment stage.
    Fragment(Admitted),
}

impl Run {
    fn module(&self) -> &Admitted {
        match self {
            Self::Kernel(kernel) => kernel.module(),
            Self::Images(module) | Self::Fragment(module) => module,
        }
    }
}

/// The corpus run on `device`, in the order of its families.
///
/// # Errors
///
/// A kernel whose capabilities and uses the device states that it refused
/// or lost, named by its family; or the corpus not made, its own defect, as
/// [`Error::Malformed`].
pub(crate) fn run(device: &Device) -> Result<Vec<Ran>, Refusal> {
    let mut answers = Vec::new();
    answers
        .try_reserve_exact(families().count())
        .map_err(|_| refusal(Error::OutOfMemory))?;
    for family in families() {
        let Entry {
            run,
            start,
            outputs,
        } = family.build().map_err(refusal)?;
        let left = left_by(device, run, &start).map_err(|why| Refusal {
            step: family.name,
            family: None,
            why,
        })?;
        answers.push(Ran {
            family: family.name,
            capabilities: family.capabilities,
            outputs,
            left,
        });
    }
    Ok(answers)
}

const fn refusal(why: Error) -> Refusal {
    Refusal {
        step: CORPUS,
        family: None,
        why,
    }
}

/// The words a kernel run as `run` left on `device`, its buffer starting as
/// `start`, run as one workgroup or one draw; `None` where the device does
/// not state what it needs.
fn left_by(device: &Device, run: Run, start: &[u8]) -> Result<Option<Vec<u32>>, Error> {
    if !device.info().runs_module(run.module()) {
        return Ok(None);
    }
    match run {
        Run::Kernel(kernel) => {
            let outcome = run_over(device, &kernel, (start, 1)).map_err(|refusal| refusal.why)?;
            match outcome {
                Outcome::Ran(left) => Ok(Some(left)),
                Outcome::Lost { loss, .. } => Err(Error::Lost(loss)),
                Outcome::Refused(why) => Err(why),
                Outcome::Unanswered => Err(Error::TimedOut),
            }
        }
        Run::Images(module) => images::dispatch(device, module, start),
        Run::Fragment(module) => fragment::draw(device, module, start),
    }
}

/// Where a family binds the image work's sampled images and its comparing
/// sampler, past its buffer, and how each is bound.
fn sampled_bindings() -> [(u32, DescriptorType); 3] {
    let [combined, depth, comparing] = crate::images::SAMPLED_KINDS;
    [(1, combined), (2, depth), (3, comparing)]
}

/// The image work's sampled images and comparing sampler, declared at
/// [`sampled_bindings`]: their types, and their variables in order.
fn declare_sampled(
    f: &mut Function<'_>,
    io: &mut Io,
) -> Result<(crate::images::SampledTypes, [Id; 3]), BuildError> {
    let types = crate::images::sampled_types(f.module(), (io.kit.uint, io.kit.float))?;
    let [(combined, _), (depth, _), (comparing, _)] = sampled_bindings();
    let variables = [
        io.bound(f, (types.combined, combined))?,
        io.bound(f, (types.depth, depth))?,
        io.bound(f, (types.comparing, comparing))?,
    ];
    Ok((types, variables))
}

/// A kernel's buffer in `context`, of memory of type `near`, holding
/// `start`; handed to `run`, then the words it holds read back.
fn through_buffer(
    context: &Context,
    near: u32,
    start: &[u8],
    run: impl FnOnce(&Buffer, u64) -> Result<(), Error>,
) -> Result<Vec<u32>, Error> {
    let size = u64::try_from(start.len()).map_err(|_| Error::TooLarge)?;
    let (memory, buffer) = crate::buffer(context, (near, size), BufferUsage::STORAGE)?;
    memory.write(0, start)?;
    run(&buffer, size)?;
    read_words(
        &memory,
        u32::try_from(size / 4).map_err(|_| Error::TooLarge)?,
    )
}

/// The layout of the bindings of `module`'s entry point for `stage`, seen
/// by `stages`: each sampled binding of the kind `sampled` names for its
/// number, every other as the module states it.
fn layout(
    module: &Admitted,
    (stage, stages): (Stage, Stages),
    sampled: &[(u32, DescriptorType)],
) -> Result<Vec<LayoutBinding>, Error> {
    let entry = module.entry_point("main", stage).ok_or(Error::EntryPoint)?;
    let mut bindings = Vec::new();
    bindings
        .try_reserve_exact(entry.bindings().len())
        .map_err(|_| Error::OutOfMemory)?;
    for binding in entry.bindings() {
        let laid = LayoutBinding::unsampled(binding, stages).or_else(|| {
            sampled
                .iter()
                .find(|&&(number, _)| number == binding.binding)
                .map(|&(_, kind)| LayoutBinding {
                    binding: binding.binding,
                    kind,
                    count: binding.count,
                    access: binding.access,
                    stages,
                })
        });
        bindings.push(laid.ok_or(Error::Malformed)?);
    }
    Ok(bindings)
}

/// Where `answered`, a device's answer to the corpus, differs from
/// `expected`, the software device's: the first word past what it is held
/// to.
///
/// # Errors
///
/// [`Difference::Corpus`] where they differ.
pub(crate) fn compare(answered: &[Ran], expected: &[Ran]) -> Result<(), Difference> {
    for (ran, reference) in answered.iter().zip(expected) {
        let (Some(left), Some(held)) = (&ran.left, &reference.left) else {
            continue;
        };
        for &(at, output) in &reference.outputs {
            let word = |words: &[u32]| {
                usize::try_from(at)
                    .ok()
                    .and_then(|index| words.get(index).copied())
            };
            let (Some(device), Some(reference)) = (word(left), word(held)) else {
                continue;
            };
            if !output.held.holds(device, reference) {
                return Err(Difference::Corpus {
                    family: ran.family,
                    case: output.case,
                    device,
                    reference,
                });
            }
        }
    }
    Ok(())
}

/// A family of the corpus: its name as a step of the work, the capabilities
/// and extensions its kernel declares past `Shader`, the version it is
/// written in, its invocations, how it runs, and what writes its body.
pub(crate) struct Family {
    name: &'static str,
    capabilities: &'static [Capability],
    extensions: &'static [&'static str],
    version: Version,
    invocations: u32,
    kind: Kind,
    body: fn(&mut Function<'_>, &mut Io) -> Result<(), BuildError>,
}

/// How a family's kernel runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    /// A compute kernel over its buffer alone.
    Kernel,
    /// A compute kernel binding the image work's images beside its buffer.
    Images,
    /// A draw's fragment stage, each of its pixels an invocation.
    Fragment,
}

impl Family {
    /// A family of one invocation over its buffer alone, written in
    /// [`VERSION`].
    pub(crate) const fn compute(
        name: &'static str,
        capabilities: &'static [Capability],
        body: fn(&mut Function<'_>, &mut Io) -> Result<(), BuildError>,
    ) -> Self {
        Self {
            name,
            capabilities,
            extensions: &[],
            version: VERSION,
            invocations: 1,
            kind: Kind::Kernel,
            body,
        }
    }

    /// The family binding the image work's images beside its buffer.
    const fn binding_images(self) -> Self {
        Self {
            kind: Kind::Images,
            ..self
        }
    }

    /// The family as a draw's fragment stage of `pixels`, declaring
    /// `extensions`.
    const fn fragment(self, pixels: u32, extensions: &'static [&'static str]) -> Self {
        Self {
            kind: Kind::Fragment,
            invocations: pixels,
            extensions,
            ..self
        }
    }

    /// The family written in `version`.
    pub(crate) const fn at(self, version: Version) -> Self {
        Self { version, ..self }
    }

    /// The family's workgroup of `invocations`.
    pub(crate) const fn across(self, invocations: u32) -> Self {
        Self {
            invocations,
            ..self
        }
    }

    /// The family's kernel, built: a module the builder, the validator or
    /// the kernel's interface refuses is the corpus's own defect,
    /// [`Error::Malformed`].
    fn build(&self) -> Result<Entry, Error> {
        let (module, io) = self.write().map_err(|refused| match refused {
            BuildError::OutOfMemory => Error::OutOfMemory,
            _ => Error::Malformed,
        })?;
        let run = match self.kind {
            Kind::Kernel => Run::Kernel(Kernel::new(module).map_err(|refused| match refused {
                crate::KernelError::OutOfMemory => Error::OutOfMemory,
                _ => Error::Malformed,
            })?),
            Kind::Images => Run::Images(module),
            Kind::Fragment => Run::Fragment(module),
        };
        let mut start = Vec::new();
        start
            .try_reserve_exact(io.start.len() * 4)
            .map_err(|_| Error::OutOfMemory)?;
        for word in &io.start {
            start.extend_from_slice(&word.to_le_bytes());
        }
        Ok(Entry {
            run,
            start,
            outputs: io.outputs,
        })
    }

    /// The family's module written, and what its buffer starts as and the
    /// words it writes.
    fn write(&self) -> Result<(Admitted, Io), BuildError> {
        let mut builder = Builder::new(self.version, 0)?;
        builder.capability(Capability::SHADER)?;
        for &capability in self.capabilities {
            builder.capability(capability)?;
        }
        for extension in self.extensions {
            builder.extension(extension)?;
        }
        let kit = Kit::declare(&mut builder)?;
        let mut io = Io::new(kit, self.invocations);
        let (void, main) = (builder.void()?, builder.id()?);
        builder.define(main, void, &[], |f, _| {
            (self.body)(f, &mut io)?;
            f.emit(Op::RETURN, &[])
        })?;
        let interface = io.interface(self.version)?;
        if self.kind == Kind::Fragment {
            builder.entry_point(ExecutionModel::FRAGMENT, main, "main", &interface)?;
            builder.execution_mode(main, ExecutionMode::ORIGIN_UPPER_LEFT, &[])?;
        } else {
            builder.entry_point(ExecutionModel::GL_COMPUTE, main, "main", &interface)?;
            builder.execution_mode(main, ExecutionMode::LOCAL_SIZE, &[self.invocations, 1, 1])?;
        }
        Ok((builder.finish()?, io))
    }
}

/// The types every kernel of the corpus declares, and its buffer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Kit {
    pub(crate) boolean: Id,
    pub(crate) uint: Id,
    pub(crate) int: Id,
    pub(crate) float: Id,
    /// Vectors of two, three and four components of each of the four, in
    /// their order.
    vectors: [[Id; 3]; 4],
    /// The buffer, its one member the runtime array of its words.
    pub(crate) words: Id,
    word: Id,
}

impl Kit {
    fn declare(b: &mut Builder) -> Result<Self, BuildError> {
        let scalars = [
            b.boolean()?,
            b.int(32, false)?,
            b.int(32, true)?,
            b.float(32)?,
        ];
        // Every slot is written below, each set its scalar's.
        let mut vectors = [[scalars[0]; 3]; 4];
        for (set, &scalar) in vectors.iter_mut().zip(&scalars) {
            for (slot, count) in set.iter_mut().zip(2..) {
                *slot = b.vector(scalar, count)?;
            }
        }
        let [boolean, uint, int, float] = scalars;
        let array = b.runtime_array(uint)?;
        b.decorate(array, Decoration::ARRAY_STRIDE, &[4])?;
        let block = b.structure(&[array])?;
        b.decorate(block, Decoration::BLOCK, &[])?;
        b.member_decorate(block, 0, Decoration::OFFSET, &[0])?;
        let buffer = b.pointer(StorageClass::STORAGE_BUFFER, block)?;
        let words = b.variable(buffer, StorageClass::STORAGE_BUFFER)?;
        b.decorate(words, Decoration::DESCRIPTOR_SET, &[0])?;
        b.decorate(words, Decoration::BINDING, &[0])?;
        let word = b.pointer(StorageClass::STORAGE_BUFFER, uint)?;
        Ok(Self {
            boolean,
            uint,
            int,
            float,
            vectors,
            words,
            word,
        })
    }

    /// The vector of `count` components of `set`: any count other than
    /// two, three or four names the widest, which the validator then
    /// refuses a vector of that many values to be built as.
    const fn pick(set: [Id; 3], count: usize) -> Id {
        match count {
            2 => set[0],
            3 => set[1],
            _ => set[2],
        }
    }

    /// The vector of `count` Booleans.
    pub(crate) const fn bvec(&self, count: usize) -> Id {
        Self::pick(self.vectors[0], count)
    }

    /// The vector of `count` unsigned integers.
    pub(crate) const fn uvec(&self, count: usize) -> Id {
        Self::pick(self.vectors[1], count)
    }

    /// The vector of `count` signed integers.
    pub(crate) const fn ivec(&self, count: usize) -> Id {
        Self::pick(self.vectors[2], count)
    }

    /// The vector of `count` singles.
    pub(crate) const fn vec(&self, count: usize) -> Id {
        Self::pick(self.vectors[3], count)
    }

    /// The scalar a vector of the kit's holds, and how many; `None` for
    /// another type.
    fn vector(&self, ty: Id) -> Option<(Id, u32)> {
        let scalars = [self.boolean, self.uint, self.int, self.float];
        scalars
            .into_iter()
            .zip(self.vectors)
            .find_map(|(component, set)| {
                let slot = set.iter().position(|&of| of == ty)?;
                [2, 3, 4].get(slot).map(|&count| (component, count))
            })
    }
}

/// A kernel's inputs and outputs as its body is written: each input a word
/// its buffer starts as, read where it is asked for, and each output a word
/// it writes after them, with how it is held; and the globals its entry
/// point lists.
pub(crate) struct Io {
    pub(crate) kit: Kit,
    lanes: u32,
    start: Vec<u32>,
    outputs: Vec<(At, Output)>,
    globals: Vec<(Id, StorageClass)>,
}

impl Io {
    const fn new(kit: Kit, lanes: u32) -> Self {
        Self {
            kit,
            lanes,
            start: Vec::new(),
            outputs: Vec::new(),
            globals: Vec::new(),
        }
    }

    /// The invocations of the kernel's workgroup.
    pub(crate) const fn lanes(&self) -> u32 {
        self.lanes
    }

    /// The pointer to the buffer's word at `index`, a word.
    fn pointer_at(&self, f: &mut Function<'_>, index: Id) -> Result<Id, BuildError> {
        let zero = f.module().uint(0)?;
        f.op(
            Op::ACCESS_CHAIN,
            self.kit.word,
            &[self.kit.words, zero, index],
        )
    }

    /// The pointer to the buffer's word `at`.
    fn pointer(&self, f: &mut Function<'_>, at: At) -> Result<Id, BuildError> {
        let index = f.module().uint(at)?;
        self.pointer_at(f, index)
    }

    /// The next word's place, holding `value` as the buffer starts.
    fn next(&mut self, value: u32) -> Result<At, BuildError> {
        let at = At::try_from(self.start.len()).map_err(|_| BuildError::OutOfMemory)?;
        self.start
            .try_reserve(1)
            .map_err(|_| BuildError::OutOfMemory)?;
        self.start.push(value);
        Ok(at)
    }

    fn record(&mut self, at: At, output: Output) -> Result<(), BuildError> {
        self.outputs
            .try_reserve(1)
            .map_err(|_| BuildError::OutOfMemory)?;
        self.outputs.push((at, output));
        Ok(())
    }

    /// An input word holding `value`, read.
    pub(crate) fn word(&mut self, f: &mut Function<'_>, value: u32) -> Result<Id, BuildError> {
        let at = self.next(value)?;
        let pointer = self.pointer(f, at)?;
        f.op(Op::LOAD, self.kit.uint, &[pointer])
    }

    /// An input signed integer.
    pub(crate) fn int(&mut self, f: &mut Function<'_>, value: i32) -> Result<Id, BuildError> {
        let word = self.word(f, value.cast_unsigned())?;
        f.op(Op::BITCAST, self.kit.int, &[word])
    }

    /// An input single.
    pub(crate) fn float(&mut self, f: &mut Function<'_>, value: f32) -> Result<Id, BuildError> {
        let word = self.word(f, value.to_bits())?;
        f.op(Op::BITCAST, self.kit.float, &[word])
    }

    /// An input Boolean.
    pub(crate) fn boolean(&mut self, f: &mut Function<'_>, value: bool) -> Result<Id, BuildError> {
        let (word, zero) = (self.word(f, u32::from(value))?, f.module().uint(0)?);
        f.op(Op::I_NOT_EQUAL, self.kit.boolean, &[word, zero])
    }

    /// An input vector of `values`, each read as `read` reads one, a value of
    /// `ty`.
    fn vector<T: Copy>(
        &mut self,
        f: &mut Function<'_>,
        (values, ty): (&[T], Id),
        read: fn(&mut Self, &mut Function<'_>, T) -> Result<Id, BuildError>,
    ) -> Result<Id, BuildError> {
        let mut parts = Vec::new();
        parts
            .try_reserve_exact(values.len())
            .map_err(|_| BuildError::OutOfMemory)?;
        for &value in values {
            parts.push(read(self, f, value)?);
        }
        f.op(Op::COMPOSITE_CONSTRUCT, ty, &parts)
    }

    /// An input vector of singles.
    pub(crate) fn floats(
        &mut self,
        f: &mut Function<'_>,
        values: &[f32],
    ) -> Result<Id, BuildError> {
        let ty = self.kit.vec(values.len());
        self.vector(f, (values, ty), Self::float)
    }

    /// An input vector of unsigned integers.
    pub(crate) fn uints(&mut self, f: &mut Function<'_>, values: &[u32]) -> Result<Id, BuildError> {
        let ty = self.kit.uvec(values.len());
        self.vector(f, (values, ty), Self::word)
    }

    /// An input vector of signed integers.
    pub(crate) fn ints(&mut self, f: &mut Function<'_>, values: &[i32]) -> Result<Id, BuildError> {
        let ty = self.kit.ivec(values.len());
        self.vector(f, (values, ty), Self::int)
    }

    /// An input vector of Booleans.
    pub(crate) fn booleans(
        &mut self,
        f: &mut Function<'_>,
        values: &[bool],
    ) -> Result<Id, BuildError> {
        let ty = self.kit.bvec(values.len());
        self.vector(f, (values, ty), Self::boolean)
    }

    /// A word of the buffer starting as `value` whose last value is `case`'s
    /// answer: the pointer to it.
    pub(crate) fn slot(
        &mut self,
        f: &mut Function<'_>,
        case: &'static str,
        value: u32,
    ) -> Result<Id, BuildError> {
        let at = self.next(value)?;
        self.record(
            at,
            Output {
                case,
                held: Held::Exact,
            },
        )?;
        self.pointer(f, at)
    }

    /// `value`, a scalar of the kit's type `ty`, as a word.
    fn as_word(&self, f: &mut Function<'_>, (value, ty): (Id, Id)) -> Result<Id, BuildError> {
        let kit = self.kit;
        if ty == kit.uint {
            Ok(value)
        } else if ty == kit.boolean {
            let (one, zero) = (f.module().uint(1)?, f.module().uint(0)?);
            f.op(Op::SELECT, kit.uint, &[value, one, zero])
        } else {
            f.op(Op::BITCAST, kit.uint, &[value])
        }
    }

    /// Write `value`, of the kit's type `ty` or a vector of it, as `case`'s
    /// answer, held as `held`.
    pub(crate) fn out(
        &mut self,
        f: &mut Function<'_>,
        (case, held): (&'static str, Held),
        (value, ty): (Id, Id),
    ) -> Result<(), BuildError> {
        if let Some((component, count)) = self.kit.vector(ty) {
            for index in 0..count {
                let part = member(f, value, component, index)?;
                self.out(f, (case, held), (part, component))?;
            }
            return Ok(());
        }
        let word = self.as_word(f, (value, ty))?;
        let at = self.next(0)?;
        let pointer = self.pointer(f, at)?;
        f.emit(Op::STORE, &[pointer, word])?;
        self.record(at, Output { case, held })
    }

    /// Write `value`, a scalar of the kit's type `ty`, as each invocation's
    /// own word of `case`'s answer, the invocation `lane` among the
    /// workgroup's.
    pub(crate) fn out_lanes(
        &mut self,
        f: &mut Function<'_>,
        (case, held): (&'static str, Held),
        (value, ty): (Id, Id),
        lane: Id,
    ) -> Result<(), BuildError> {
        let first = self.next(0)?;
        self.record(first, Output { case, held })?;
        for _ in 1..self.lanes {
            let at = self.next(0)?;
            self.record(at, Output { case, held })?;
        }
        let word = self.as_word(f, (value, ty))?;
        let base = f.module().uint(first)?;
        let index = f.op(Op::I_ADD, self.kit.uint, &[base, lane])?;
        let pointer = self.pointer_at(f, index)?;
        f.emit(Op::STORE, &[pointer, word])
    }

    /// A global variable of `ty` in `class`, listed by the entry point where
    /// the kernel's version asks it be.
    pub(crate) fn global(
        &mut self,
        f: &mut Function<'_>,
        (ty, class): (Id, StorageClass),
    ) -> Result<Id, BuildError> {
        let pointer = f.module().pointer(class, ty)?;
        let variable = f.module().variable(pointer, class)?;
        self.globals
            .try_reserve(1)
            .map_err(|_| BuildError::OutOfMemory)?;
        self.globals.push((variable, class));
        Ok(variable)
    }

    /// The built-in `built_in`, a value of `ty`, read.
    pub(crate) fn built_in(
        &mut self,
        f: &mut Function<'_>,
        (built_in, ty): (BuiltIn, Id),
    ) -> Result<Id, BuildError> {
        let variable = self.global(f, (ty, StorageClass::INPUT))?;
        f.module()
            .decorate(variable, Decoration::BUILT_IN, &[built_in.0])?;
        f.op(Op::LOAD, ty, &[variable])
    }

    /// A global of `ty` bound at set 0, `binding`.
    pub(crate) fn bound(
        &mut self,
        f: &mut Function<'_>,
        (ty, binding): (Id, u32),
    ) -> Result<Id, BuildError> {
        let variable = self.global(f, (ty, StorageClass::UNIFORM_CONSTANT))?;
        f.module()
            .decorate(variable, Decoration::DESCRIPTOR_SET, &[0])?;
        f.module()
            .decorate(variable, Decoration::BINDING, &[binding])?;
        Ok(variable)
    }

    /// The globals the entry point lists at `version`: its inputs and
    /// outputs, and from 1.4 every global it uses.
    fn interface(&self, version: Version) -> Result<Vec<Id>, BuildError> {
        let every = version >= LISTS_EVERY_GLOBAL;
        let mut listed = Vec::new();
        listed
            .try_reserve_exact(self.globals.len() + 1)
            .map_err(|_| BuildError::OutOfMemory)?;
        if every {
            listed.push(self.kit.words);
        }
        listed.extend(
            self.globals
                .iter()
                .filter(|&&(_, class)| {
                    every || class == StorageClass::INPUT || class == StorageClass::OUTPUT
                })
                .map(|&(variable, _)| variable),
        );
        Ok(listed)
    }
}

/// `case`, held bit for bit.
const fn exact(case: &'static str) -> (&'static str, Held) {
    (case, Held::Exact)
}

/// One instruction a family writes the answer of: its case, its op, its
/// result's type, and its operands.
type Row<'a> = (&'static str, Op, Id, &'a [Id]);

/// Each of `instructions`, its answer held as `held`.
fn rows(
    f: &mut Function<'_>,
    io: &mut Io,
    held: Held,
    instructions: &[Row<'_>],
) -> Result<(), BuildError> {
    for &(case, code, ty, operands) in instructions {
        op(f, io, (case, held), (code, ty, operands))?;
    }
    Ok(())
}

/// `op` over `operands`, a value of `ty`, written as `case`'s answer.
fn op(
    f: &mut Function<'_>,
    io: &mut Io,
    (case, held): (&'static str, Held),
    (op, ty, operands): (Op, Id, &[Id]),
) -> Result<(), BuildError> {
    let value = f.op(op, ty, operands)?;
    io.out(f, (case, held), (value, ty))
}

/// Member `index` of the composite `value`, of type `ty`.
fn member(f: &mut Function<'_>, value: Id, ty: Id, index: u32) -> Result<Id, BuildError> {
    f.op_args(
        Op::COMPOSITE_EXTRACT,
        ty,
        &[Arg::Id(value), Arg::Word(index)],
    )
}

/// A `Function` variable of `ty`.
fn local(f: &mut Function<'_>, ty: Id) -> Result<Id, BuildError> {
    let pointer = f.module().pointer(StorageClass::FUNCTION, ty)?;
    f.local(pointer)
}

#[cfg(test)]
#[path = "corpus_tests.rs"]
mod tests;
