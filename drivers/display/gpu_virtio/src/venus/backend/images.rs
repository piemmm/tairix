//! Images, their views, texel buffers and samplers in a Venus context: each
//! made in an exchange of its own and named only once answered, and let go
//! behind the context's work. An image is kept in the general layout for every
//! use, taken there and cleared as it is placed, so it reads zero from then
//! on whatever its memory held.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_gpu::{
    AddressMode, Aspects, BorderColor, BufferImageCopy, BufferViewDesc, ClearValue, CompareOp,
    Error, Filter, ImageCopy, ImageDesc, ImageDimension, ImageRange, ImageSubresource, ImageUsage,
    ImageViewDesc, MemoryRequirements, SamplerDesc, ViewDimension,
};
use tairix_spirv::Component;

use super::super::encode::{
    BufferImageRegion, Encoder, FormatQuery, HostClear, ImageInfo, ImageRegion, Layers,
    SamplerInfo, Sharing, Subresources, ViewInfo,
};
use super::super::info::{host_format, HostFormat, Statement};
use super::super::reply::{HostFormatFeatures, HostImageLimits, Reader};
use super::super::{aspect, format_feature, image, Command};
use super::{
    device_error, lose, queue_commands, succeeded, utility, VenusContext, VenusMemory, DEVICE,
    MADE_REPLY, PHYSICAL, SHORT_REPLY, UTILITY,
};

/// An image made in a context: its id and how it was made.
pub(crate) struct VenusImage {
    pub(super) context: Arc<VenusContext>,
    pub(super) image: u64,
    pub(super) desc: ImageDesc,
    /// The uses it was made for, as the host names them.
    usage: u32,
    /// Whether it was made able to be seen as a cube.
    cube: bool,
}

impl core::fmt::Debug for VenusImage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VenusImage")
            .field("image", &self.image)
            .finish_non_exhaustive()
    }
}

impl Drop for VenusImage {
    fn drop(&mut self) {
        release(&self.context, &[(Command::DestroyImage, self.image)]);
    }
}

/// A view of an image made in a context.
pub(crate) struct VenusView {
    context: Arc<VenusContext>,
    pub(super) view: u64,
}

impl core::fmt::Debug for VenusView {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VenusView")
            .field("view", &self.view)
            .finish_non_exhaustive()
    }
}

impl Drop for VenusView {
    fn drop(&mut self) {
        release(&self.context, &[(Command::DestroyImageView, self.view)]);
    }
}

/// A texel buffer made in a context.
pub(crate) struct VenusTexels {
    context: Arc<VenusContext>,
    pub(super) view: u64,
}

impl core::fmt::Debug for VenusTexels {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VenusTexels")
            .field("view", &self.view)
            .finish_non_exhaustive()
    }
}

impl Drop for VenusTexels {
    fn drop(&mut self) {
        release(&self.context, &[(Command::DestroyBufferView, self.view)]);
    }
}

/// A sampler made in a context: the one reading its border as floats, and
/// where it clamps to its border, the one reading it as integers.
pub(crate) struct VenusSampler {
    context: Arc<VenusContext>,
    float: u64,
    integer: Option<u64>,
}

impl VenusSampler {
    /// The sampler a binding reading `integer` texels or not names.
    pub(super) fn of(&self, integer: bool) -> u64 {
        match self.integer {
            Some(sampler) if integer => sampler,
            _ => self.float,
        }
    }
}

impl core::fmt::Debug for VenusSampler {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VenusSampler")
            .field("float", &self.float)
            .finish_non_exhaustive()
    }
}

impl Drop for VenusSampler {
    fn drop(&mut self) {
        let mut gone: [(Command, u64); 2] = [(Command::DestroySampler, self.float); 2];
        let count = match self.integer {
            Some(integer) => {
                gone[1].1 = integer;
                2
            }
            None => 1,
        };
        release(&self.context, &gone[..count]);
    }
}

/// Let go of each object `gone` names behind the context's work; a context
/// that cannot say so is lost, its end letting go of everything.
fn release(context: &Arc<VenusContext>, gone: &[(Command, u64)]) {
    let mut encoder = Encoder::default();
    let said = gone
        .iter()
        .try_for_each(|&(command, id)| encoder.destroy(command, DEVICE, id));
    match said {
        Ok(()) => queue_commands(context, encoder.as_bytes()),
        Err(_) => lose(context),
    }
}

/// The seam's format as the host holds it.
pub(super) fn held(
    statement: &Statement,
    format: tairix_gpu::Format,
) -> Result<HostFormat, DriverError> {
    statement
        .formats
        .get(format.index())
        .copied()
        .ok_or(DriverError::OutOfRange)
}

/// The host's planes of the seam's.
fn aspects_of(aspects: Aspects) -> u32 {
    [
        (Aspects::COLOR, aspect::COLOR),
        (Aspects::DEPTH, aspect::DEPTH),
        (Aspects::STENCIL, aspect::STENCIL),
    ]
    .iter()
    .filter(|&&(seam, _)| aspects.contains(seam))
    .fold(0, |bits, &(_, host)| bits | host)
}

/// The host's uses of the seam's, and the clear every image is placed with.
fn usage_of(usage: ImageUsage) -> u32 {
    [
        (ImageUsage::TRANSFER_SRC, image::TRANSFER_SRC),
        (ImageUsage::TRANSFER_DST, image::TRANSFER_DST),
        (ImageUsage::SAMPLED, image::SAMPLED),
        (ImageUsage::STORAGE, image::STORAGE),
        (ImageUsage::COLOR_ATTACHMENT, image::COLOR_ATTACHMENT),
        (
            ImageUsage::DEPTH_STENCIL_ATTACHMENT,
            image::DEPTH_STENCIL_ATTACHMENT,
        ),
    ]
    .iter()
    .filter(|&&(seam, _)| usage.contains(seam))
    .fold(image::TRANSFER_DST, |bits, &(_, host)| bits | host)
}

/// Whether an image of `desc` may be seen as a cube: a square of one sample,
/// with the layers.
pub(super) const fn cubed(desc: &ImageDesc) -> bool {
    matches!(desc.dimension, ImageDimension::D2)
        && desc.samples == 1
        && desc.extent.width == desc.extent.height
        && desc.array_layers >= 6
}

/// How an image of `desc`, held as `held`, is made: able to be seen as a
/// cube where it is [`cubed`], and as its sRGB twin where it has one, in a
/// list of the two where the host takes one.
fn image_info(
    statement: &Statement,
    desc: &ImageDesc,
    held: HostFormat,
) -> Result<ImageInfo, DriverError> {
    let (mut flags, view_formats) = twinned(desc.format, statement.format_lists, |format| {
        Ok(self::held(statement, format)?.format)
    })?;
    if cubed(desc) {
        flags |= image::CUBE_COMPATIBLE;
    }
    Ok(ImageInfo {
        flags,
        kind: match desc.dimension {
            ImageDimension::D1 => 0,
            ImageDimension::D2 => 1,
            ImageDimension::D3 => 2,
        },
        format: held.format,
        extent: [desc.extent.width, desc.extent.height, desc.extent.depth],
        levels: desc.mip_levels,
        layers: desc.array_layers,
        samples: i32::try_from(desc.samples).map_err(|_| DriverError::OutOfRange)?,
        usage: usage_of(desc.usage),
        view_formats,
        // Only an image made for copies reaches a copy engine; the rest stay
        // the first family's, its clear on placing them run there.
        sharing: if desc
            .usage
            .intersects(ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST))
        {
            statement.sharing
        } else {
            Sharing::default()
        },
    })
}

/// The flags an image of `format` is made with to be seen as its sRGB twin,
/// where it has one, and the list of the two its views read it as, the host's
/// formats as `host` names them, where `lists`.
fn twinned(
    format: tairix_gpu::Format,
    lists: bool,
    host: impl Fn(tairix_gpu::Format) -> Result<i32, DriverError>,
) -> Result<(u32, Option<[i32; 2]>), DriverError> {
    match format.srgb_twin() {
        Some(twin) => Ok((
            image::MUTABLE_FORMAT,
            if lists {
                Some([host(format)?, host(twin)?])
            } else {
                None
            },
        )),
        None => Ok((0, None)),
    }
}

/// How the host is asked what it makes of the widest image of four samples
/// the seam makes of `format`, offering `offered`, its formats listed where
/// `lists`: a two-dimensional target of it, sampled where the format is,
/// copied both ways; none where the format is no target.
pub(super) fn four_samples(
    format: tairix_gpu::Format,
    offered: HostFormatFeatures,
    lists: bool,
) -> Result<Option<FormatQuery>, DriverError> {
    let target = if format.aspects() == Aspects::COLOR {
        (format_feature::COLOR_ATTACHMENT, image::COLOR_ATTACHMENT)
    } else {
        (
            format_feature::DEPTH_STENCIL_ATTACHMENT,
            image::DEPTH_STENCIL_ATTACHMENT,
        )
    };
    if offered.optimal & target.0 == 0 {
        return Ok(None);
    }
    let mut usage = image::TRANSFER_SRC | image::TRANSFER_DST | target.1;
    if offered.optimal & format_feature::SAMPLED_IMAGE != 0 {
        usage |= image::SAMPLED;
    }
    let (flags, view_formats) = twinned(format, lists, |format| Ok(host_format(format)))?;
    Ok(Some(FormatQuery {
        format: host_format(format),
        kind: 1,
        usage,
        flags,
        view_formats,
    }))
}

/// Whether the host makes an image of `desc`, made as `info`, asked through
/// `encoder`.
fn makes(
    context: &VenusContext,
    encoder: &mut Encoder,
    desc: &ImageDesc,
    info: &ImageInfo,
) -> Result<bool, DriverError> {
    encoder.clear();
    encoder.get_physical_device_image_format_properties2(
        PHYSICAL,
        &FormatQuery {
            format: info.format,
            kind: info.kind,
            usage: info.usage,
            flags: info.flags,
            view_formats: info.view_formats,
        },
    )?;
    let most = Reader::new(&context.exchange(encoder.as_bytes(), 0, SHORT_REPLY)?)
        .image_format_properties()?;
    Ok(most.is_some_and(|most| within(desc, &most)))
}

/// Whether an image of `desc` is within what the host makes of its format,
/// kind, use and flags.
fn within(desc: &ImageDesc, most: &HostImageLimits) -> bool {
    let extent = [desc.extent.width, desc.extent.height, desc.extent.depth];
    extent
        .iter()
        .zip(&most.extent)
        .all(|(side, most)| side <= most)
        && desc.mip_levels <= most.levels
        && desc.array_layers <= most.layers
        && most.samples & desc.samples != 0
}

/// Make an image of `desc` in `context`, which the host is asked first it
/// makes, and what it needs of the memory it is placed in: asked with its
/// making where the host answers it of the description.
pub(super) fn make_image(
    context: &Arc<VenusContext>,
    desc: &ImageDesc,
) -> Result<(VenusImage, MemoryRequirements), DriverError> {
    let statement = &context.core.statement;
    let held = held(statement, desc.format)?;
    if !held.copied() {
        return Err(DriverError::Unsupported);
    }
    let mut info = image_info(statement, desc, held)?;
    let mut encoder = Encoder::default();
    let mut made = makes(context, &mut encoder, desc, &info)?;
    if !made && info.flags & image::CUBE_COMPATIBLE != 0 {
        // A host may make an image it does not make able to be seen as a
        // cube, which is then seen as anything else.
        info.flags &= !image::CUBE_COMPATIBLE;
        made = makes(context, &mut encoder, desc, &info)?;
    }
    if !made {
        return Err(DriverError::Unsupported);
    }
    let [id] = context.ids();
    encoder.clear();
    let asked_first = statement.device_requirements;
    if asked_first {
        encoder.get_device_image_memory_requirements(DEVICE, &info)?;
    }
    encoder.create_image(DEVICE, id, &info)?;
    let room = if asked_first { SHORT_REPLY } else { 0 } + MADE_REPLY;
    let replies = context.exchange(encoder.as_bytes(), 0, room)?;
    let mut reader = Reader::new(&replies);
    let asked = asked_first
        .then(|| reader.memory_requirements(Command::GetDeviceImageMemoryRequirements))
        .transpose()?;
    succeeded(reader.made(Command::CreateImage, id)?)?;
    let made = VenusImage {
        context: Arc::clone(context),
        image: id,
        desc: *desc,
        usage: info.usage,
        cube: info.flags & image::CUBE_COMPATIBLE != 0,
    };
    let required = if let Some(required) = asked {
        required
    } else {
        // An image's requirements name it, so they wait on its making.
        encoder.clear();
        encoder.get_image_memory_requirements2(DEVICE, id)?;
        Reader::new(&context.exchange(encoder.as_bytes(), 0, SHORT_REPLY)?)
            .memory_requirements(Command::GetImageMemoryRequirements2)?
    };
    let requirements = statement.image_requirements(required)?;
    Ok((made, requirements))
}

/// Every plane, level and layer of an image of `desc`.
fn whole(desc: &ImageDesc) -> Subresources {
    Subresources {
        aspects: aspects_of(desc.format.aspects()),
        base_level: 0,
        levels: desc.mip_levels,
        base_layer: 0,
        layers: desc.array_layers,
    }
}

/// Bind `image` at `offset` into `memory`, then take it to the general layout
/// and clear every texel, answering once the device has.
pub(super) fn place_image(
    image: &VenusImage,
    memory: &VenusMemory,
    offset: u64,
) -> Result<(), DriverError> {
    let context = &image.context;
    // Each context is a host device of its own, reaching no other's memory.
    if !Arc::ptr_eq(context, &memory.context) {
        return Err(DriverError::Unsupported);
    }
    let mut encoder = Encoder::default();
    encoder.bind_image_memory2(DEVICE, image.image, (memory.memory, offset))?;
    succeeded(
        Reader::new(&context.exchange(encoder.as_bytes(), 0, SHORT_REPLY)?)
            .result_of(Command::BindImageMemory2)?,
    )?;
    let range = whole(&image.desc);
    utility(context, |encoder| {
        encoder.cmd_image_kept(UTILITY, image.image, &range)?;
        if image.desc.format.aspects() == Aspects::COLOR {
            let kind = clear_kind(image.desc.format.component());
            encoder.cmd_clear_color_image(UTILITY, image.image, (kind, [0; 4]), &[range])
        } else {
            encoder.cmd_clear_depth_stencil_image(UTILITY, image.image, (0.0, 0), &[range])
        }
    })
}

/// How a clear of a format of `component` reads its colour: as floats,
/// signed or unsigned integers.
const fn clear_kind(component: Component) -> u32 {
    match component {
        Component::Float => 0,
        Component::Sint => 1,
        Component::Uint => 2,
    }
}

/// Make a view of `desc` of `image`, made for the uses of its image its
/// format offers.
pub(super) fn make_view(
    image: &VenusImage,
    desc: &ImageViewDesc,
) -> Result<VenusView, DriverError> {
    let context = &image.context;
    let cube = matches!(
        desc.dimension,
        ViewDimension::Cube | ViewDimension::CubeArray
    );
    if cube && !image.cube {
        return Err(DriverError::Unsupported);
    }
    let held = held(&context.core.statement, desc.format)?;
    let offers = |feature: u64| held.features.optimal & feature == feature;
    let mut usage = image.usage
        & (image::SAMPLED
            | image::STORAGE
            | image::COLOR_ATTACHMENT
            | image::DEPTH_STENCIL_ATTACHMENT);
    if !offers(format_feature::SAMPLED_IMAGE) {
        usage &= !image::SAMPLED;
    }
    if !offers(format_feature::STORAGE_IMAGE) {
        usage &= !image::STORAGE;
    }
    if !offers(format_feature::COLOR_ATTACHMENT) {
        usage &= !image::COLOR_ATTACHMENT;
    }
    if !offers(format_feature::DEPTH_STENCIL_ATTACHMENT) {
        usage &= !image::DEPTH_STENCIL_ATTACHMENT;
    }
    if usage == 0 {
        return Err(DriverError::Unsupported);
    }
    let range = &desc.range;
    let info = ViewInfo {
        image: image.image,
        kind: match desc.dimension {
            ViewDimension::D1 => 0,
            ViewDimension::D2 => 1,
            ViewDimension::D3 => 2,
            ViewDimension::Cube => 3,
            ViewDimension::D1Array => 4,
            ViewDimension::D2Array => 5,
            ViewDimension::CubeArray => 6,
        },
        format: held.format,
        range: Subresources {
            aspects: aspects_of(range.aspects),
            base_level: range.base_level,
            levels: range.levels,
            base_layer: range.base_layer,
            layers: range.layers,
        },
        usage,
    };
    let [id] = context.ids();
    let mut encoder = Encoder::default();
    encoder.create_image_view(DEVICE, id, &info)?;
    succeeded(
        Reader::new(&context.exchange(encoder.as_bytes(), 0, MADE_REPLY)?)
            .made(Command::CreateImageView, id)?,
    )?;
    Ok(VenusView {
        context: Arc::clone(context),
        view: id,
    })
}

/// Make a texel buffer of `desc` at `offset` into `memory`.
pub(super) fn make_texels(
    memory: &VenusMemory,
    offset: u64,
    desc: &BufferViewDesc,
) -> Result<VenusTexels, DriverError> {
    let context = &memory.context;
    let format = held(&context.core.statement, desc.format)?.format;
    let [id] = context.ids();
    let mut encoder = Encoder::default();
    encoder.create_buffer_view(DEVICE, id, (memory.buffer, format), (offset, desc.size))?;
    succeeded(
        Reader::new(&context.exchange(encoder.as_bytes(), 0, MADE_REPLY)?)
            .made(Command::CreateBufferView, id)?,
    )?;
    Ok(VenusTexels {
        context: Arc::clone(context),
        view: id,
    })
}

/// The host's name of a filter.
const fn filter(filter: Filter) -> i32 {
    match filter {
        Filter::Nearest => 0,
        Filter::Linear => 1,
    }
}

/// The host's name of an address mode.
const fn address(mode: AddressMode) -> i32 {
    match mode {
        AddressMode::Repeat => 0,
        AddressMode::MirroredRepeat => 1,
        AddressMode::ClampToEdge => 2,
        AddressMode::ClampToBorder => 3,
        AddressMode::MirrorClampToEdge => 4,
    }
}

/// The host's name of a comparison.
pub(super) const fn compare(op: CompareOp) -> i32 {
    match op {
        CompareOp::Never => 0,
        CompareOp::Less => 1,
        CompareOp::Equal => 2,
        CompareOp::LessOrEqual => 3,
        CompareOp::Greater => 4,
        CompareOp::NotEqual => 5,
        CompareOp::GreaterOrEqual => 6,
        CompareOp::Always => 7,
    }
}

/// The host's name of a border colour, read as integers or floats.
const fn border(color: BorderColor, integer: bool) -> i32 {
    let float = match color {
        BorderColor::TransparentBlack => 0,
        BorderColor::OpaqueBlack => 2,
        BorderColor::OpaqueWhite => 4,
    };
    if integer {
        float + 1
    } else {
        float
    }
}

/// Make a sampler of `desc` in `context`: one reading its border as floats,
/// and where it clamps to its border, another reading it as integers.
pub(super) fn make_sampler(
    context: &Arc<VenusContext>,
    desc: &SamplerDesc,
) -> Result<VenusSampler, DriverError> {
    let anisotropy = if desc.max_anisotropy > 1 {
        let most = u16::try_from(desc.max_anisotropy).map_err(|_| DriverError::OutOfRange)?;
        Some(f32::from(most))
    } else {
        None
    };
    let info = |integer| SamplerInfo {
        filters: [
            filter(desc.mag_filter),
            filter(desc.min_filter),
            filter(desc.mipmap_filter),
        ],
        address: desc.address.map(address),
        lod_bias: desc.lod_bias,
        anisotropy,
        compare: desc.compare.map(compare),
        lod: [desc.lod_min, desc.lod_max],
        border: border(desc.border, integer),
        unnormalized: desc.unnormalized,
    };
    // The two name nothing of each other, so they travel in one exchange.
    let bordered = desc.address.contains(&AddressMode::ClampToBorder);
    let [float, integer] = context.ids();
    let mut encoder = Encoder::default();
    encoder.create_sampler(DEVICE, float, &info(false))?;
    if bordered {
        encoder.create_sampler(DEVICE, integer, &info(true))?;
    }
    let room = MADE_REPLY * (1 + usize::from(bordered));
    let replies = context.exchange(encoder.as_bytes(), 0, room)?;
    let mut reader = Reader::new(&replies);
    let float_made = succeeded(reader.made(Command::CreateSampler, float)?);
    let integer_made = bordered
        .then(|| reader.made(Command::CreateSampler, integer))
        .transpose()?
        .map(succeeded);
    match (float_made, integer_made) {
        (Ok(()), None) => Ok(VenusSampler {
            context: Arc::clone(context),
            float,
            integer: None,
        }),
        (Ok(()), Some(Ok(()))) => Ok(VenusSampler {
            context: Arc::clone(context),
            float,
            integer: Some(integer),
        }),
        (Ok(()), Some(Err(refused))) => {
            release(context, &[(Command::DestroySampler, float)]);
            Err(refused)
        }
        (Err(refused), Some(Ok(()))) => {
            release(context, &[(Command::DestroySampler, integer)]);
            Err(refused)
        }
        (Err(refused), _) => Err(refused),
    }
}

/// One plane, level and run of layers of an image, as the host names it.
fn layers_of(part: &ImageSubresource) -> Layers {
    Layers {
        aspect: aspects_of(part.aspect),
        level: part.level,
        base_layer: part.base_layer,
        count: part.layers,
    }
}

/// A texel's place as the host names it.
fn at(origin: tairix_gpu::Origin3d) -> Result<[i32; 3], DriverError> {
    let axis = |at: u32| i32::try_from(at).map_err(|_| DriverError::OutOfRange);
    Ok([axis(origin.x)?, axis(origin.y)?, axis(origin.z)?])
}

/// `regions` of a copy between an image of `desc` and a buffer starting
/// `start` bytes into its memory, as the host names them: rows of texels.
pub(super) fn buffer_image_regions(
    desc: &ImageDesc,
    start: u64,
    regions: &[BufferImageCopy],
) -> Result<Vec<BufferImageRegion>, DriverError> {
    let mut named = Vec::new();
    named
        .try_reserve_exact(regions.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    for region in regions {
        let texel = desc
            .format
            .copy_bytes(region.image.aspect)
            .ok_or(DriverError::DeviceFault)?;
        named.push(BufferImageRegion {
            offset: start
                .checked_add(region.buffer_offset)
                .ok_or(DriverError::OutOfRange)?,
            row_length: region.bytes_per_row / texel,
            image_height: region.rows_per_image,
            layers: layers_of(&region.image),
            at: at(region.origin)?,
            extent: [
                region.extent.width,
                region.extent.height,
                region.extent.depth,
            ],
        });
    }
    Ok(named)
}

/// `regions` of a copy between images, as the host names them.
pub(super) fn image_regions(regions: &[ImageCopy]) -> Result<Vec<ImageRegion>, DriverError> {
    let mut named = Vec::new();
    named
        .try_reserve_exact(regions.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    for region in regions {
        named.push(ImageRegion {
            src: (layers_of(&region.src), at(region.src_origin)?),
            dst: (layers_of(&region.dst), at(region.dst_origin)?),
            extent: [
                region.extent.width,
                region.extent.height,
                region.extent.depth,
            ],
        });
    }
    Ok(named)
}

/// Clear `ranges` of `image` to `value` into `encoder`, recording into
/// `commands`.
pub(super) fn clear(
    encoder: &mut Encoder,
    commands: u64,
    image: &VenusImage,
    (value, ranges): (ClearValue, &[ImageRange]),
) -> Result<(), DriverError> {
    let mut named = Vec::new();
    named
        .try_reserve_exact(ranges.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    named.extend(ranges.iter().map(|range| Subresources {
        aspects: aspects_of(range.aspects),
        base_level: range.base_level,
        levels: range.levels,
        base_layer: range.base_layer,
        layers: range.layers,
    }));
    let id = image.image;
    match host_clear(value, image.desc.format) {
        HostClear::Colour(kind, bits) => {
            encoder.cmd_clear_color_image(commands, id, (kind, bits), &named)
        }
        HostClear::DepthStencil(depth, stencil) => {
            encoder.cmd_clear_depth_stencil_image(commands, id, (depth, stencil), &named)
        }
    }
}

/// `value` as the host clears `format` to: of an integer format what its
/// channels keep of it, as Vulkan defines no integer value they cannot hold.
pub(super) fn host_clear(value: ClearValue, format: tairix_gpu::Format) -> HostClear {
    let kind = clear_kind(format.component());
    match value.held(format) {
        ClearValue::Float(channels) => HostClear::Colour(kind, channels.map(f32::to_bits)),
        ClearValue::Sint(channels) => HostClear::Colour(kind, channels.map(i32::cast_unsigned)),
        ClearValue::Uint(channels) => HostClear::Colour(kind, channels),
        ClearValue::DepthStencil { depth, stencil } => {
            HostClear::DepthStencil(depth, u32::from(stencil))
        }
    }
}

/// `refused`, an image's or a view's making refused, as the seam names it,
/// losing the context where the device faulted.
pub(super) fn refused(context: &Arc<VenusContext>, refused: DriverError) -> Error {
    if refused == DriverError::DeviceFault {
        lose(context);
    }
    device_error(refused)
}
