//! The scene `gpucheck --present` shows (`plans/GPU.md` GP6.5): a ground, a
//! band, a row of tiles and a tile copied within the picture, drawn by a clear
//! and copies the device runs, at whatever extent the window has.
//!
//! Every colour is written exactly — the clear's components are nought or
//! one, and everything else is copied — so any device leaves the same bytes.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::time::MonotonicClock;
use tairix_gpu::{
    Allocation, AllocationDesc, Aspects, Buffer, BufferDesc, BufferImageCopy, BufferUsage,
    ClearValue, CommandBuffer, Context, ContextDesc, Device, Error, Extent3d, Format, Image,
    ImageCopy, ImageDesc, ImageDimension, ImageRange, ImageSubresource, ImageUsage, Origin3d,
};

use crate::{clearing, near, software, submit, REFERENCE_MEMORY};

/// The ground, as the clear writes it: opaque blue.
const GROUND: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
/// Rows the band spans at most.
const BAND_ROWS: u32 = 64;
/// The side of a tile.
const TILE: u32 = 32;
/// The side of one of a tile's checks.
const CHECK: u32 = 4;
/// How many tiles the row holds at most.
const TILES: u32 = 8;
/// Pixels from one tile's left edge to the next's.
const TILE_STEP: u32 = 96;
/// The colours a tile's checks alternate between.
const CHECKS: [[u8; 3]; 2] = [[0xF0, 0x40, 0x20], [0x20, 0xC0, 0xF0]];
/// Pixels between the copied tile and the picture's far corner.
const MARGIN: u32 = 8;

/// How a picture lays a pixel's bytes out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Order {
    Rgba,
    Bgra,
}

impl Order {
    fn of(format: Format) -> Result<Self, Error> {
        match format {
            Format::Rgba8Unorm => Ok(Self::Rgba),
            Format::Bgra8Unorm => Ok(Self::Bgra),
            _ => Err(Error::Unsupported),
        }
    }

    /// The opaque colour `[r, g, b]` as this order's bytes.
    const fn bytes(self, [r, g, b]: [u8; 3]) -> [u8; 4] {
        match self {
            Self::Rgba => [r, g, b, 0xFF],
            Self::Bgra => [b, g, r, 0xFF],
        }
    }
}

/// Where the scene places its parts in a picture of one extent.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Layout {
    extent: (u32, u32),
    /// The band's first row and rows.
    band: Option<(u32, u32)>,
    /// Each tile's top-left corner.
    tiles: Vec<(u32, u32)>,
    /// Where the first tile is copied to.
    copy: Option<(u32, u32)>,
}

impl Layout {
    fn of((width, height): (u32, u32)) -> Self {
        let top = height / 8;
        let rows = BAND_ROWS.min(height - top);
        let band = (width > 0 && rows > 0).then_some((top, rows));
        let row = height / 2;
        let tiles: Vec<(u32, u32)> = (0..TILES)
            .map(|at| (TILE + at * TILE_STEP, row))
            .take_while(|&(x, y)| x + TILE <= width && y + TILE <= height)
            .collect();
        let copy = tiles.first().and_then(|&(x, y)| {
            let to = (
                width.checked_sub(TILE + MARGIN)?,
                height.checked_sub(TILE + MARGIN)?,
            );
            let apart = to.0 >= x + TILE || to.1 >= y + TILE;
            apart.then_some(to)
        });
        Self {
            extent: (width, height),
            band,
            tiles,
            copy,
        }
    }

    /// The band's colour at column `x` of its row `row`.
    fn band_colour(&self, x: u32, row: u32) -> [u8; 3] {
        let span = |extent: u32| extent.saturating_sub(1).max(1);
        let rows = self.band.map_or(1, |(_, rows)| rows);
        let level = |at: u32, extent: u32| u8::try_from(at * 255 / span(extent)).unwrap_or(u8::MAX);
        [level(x, self.extent.0), level(row, rows), 0x80]
    }

    /// A tile's colour at its pixel `(x, y)`.
    fn tile_colour(x: u32, y: u32) -> [u8; 3] {
        CHECKS[usize::from((x / CHECK + y / CHECK) % 2 == 1)]
    }

    /// The bytes the copies read: the band's rows, then one tile's.
    ///
    /// # Errors
    ///
    /// [`Error::TooLarge`] for more bytes than an address reaches, and
    /// [`Error::OutOfMemory`] where the host refuses them.
    fn source(&self, order: Order) -> Result<Vec<u8>, Error> {
        let len = self.tile_offset() + u64::from(TILE * TILE) * 4;
        let mut bytes = room_for(usize::try_from(len).map_err(|_| Error::TooLarge)?)?;
        if let Some((_, rows)) = self.band {
            for row in 0..rows {
                for x in 0..self.extent.0 {
                    bytes.extend_from_slice(&order.bytes(self.band_colour(x, row)));
                }
            }
        }
        for y in 0..TILE {
            for x in 0..TILE {
                bytes.extend_from_slice(&order.bytes(Self::tile_colour(x, y)));
            }
        }
        Ok(bytes)
    }

    /// Where the tile's bytes start among [`Self::source`]'s.
    fn tile_offset(&self) -> u64 {
        self.band.map_or(0, |(_, rows)| {
            u64::from(self.extent.0) * u64::from(rows) * 4
        })
    }
}

/// An empty buffer with room for exactly `len` bytes.
///
/// # Errors
///
/// [`Error::OutOfMemory`] where the host refuses them.
fn room_for(len: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| Error::OutOfMemory)?;
    Ok(bytes)
}

/// The colour aspect of a picture's one level and layer.
const COLOUR: ImageSubresource = ImageSubresource {
    aspect: Aspects::COLOR,
    level: 0,
    base_layer: 0,
    layers: 1,
};

/// The point `(x, y)` of a picture's one layer.
const fn origin(x: u32, y: u32) -> Origin3d {
    Origin3d { x, y, z: 0 }
}

/// `width` by `height` pixels of one layer.
const fn extent3d(width: u32, height: u32) -> Extent3d {
    Extent3d {
        width,
        height,
        depth: 1,
    }
}

/// The scene at one extent, ready to be recorded into a picture of it: the
/// bytes its copies read, in memory the device holds.
pub struct Scene {
    layout: Layout,
    _memory: Allocation,
    source: Buffer,
}

impl Scene {
    /// The scene at `extent` for a picture in `format`, its bytes written into
    /// host-visible memory of `context` on `device`.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for a format other than four 8-bit unsigned
    /// normalised channels, [`Error::NoSuchMemoryType`] for a device the CPU
    /// can write none of, [`Error::TooLarge`] or [`Error::OutOfMemory`] where
    /// the host cannot hold the bytes written, and the device's refusal of the
    /// memory or buffer.
    pub fn new(
        device: &Device,
        context: &Context,
        extent: (u32, u32),
        format: Format,
    ) -> Result<Self, Error> {
        let order = Order::of(format)?;
        let layout = Layout::of(extent);
        let bytes = layout.source(order)?;
        let size = u64::try_from(bytes.len()).map_err(|_| Error::TooLarge)?;
        let near = near(device)?;
        let memory = context.allocate(&AllocationDesc {
            size,
            memory_type: near,
        })?;
        memory.write(0, &bytes)?;
        let source = context.create_buffer(
            &memory,
            &BufferDesc {
                offset: 0,
                size,
                usage: BufferUsage::TRANSFER_SRC,
            },
        )?;
        Ok(Self {
            layout,
            _memory: memory,
            source,
        })
    }

    /// Record the scene into `image`, a picture of the extent and format it
    /// was made for, on `commands`.
    ///
    /// # Errors
    ///
    /// What recording refuses: a picture or buffer of another context.
    pub fn record(&self, commands: &mut CommandBuffer, image: &Image) -> Result<(), Error> {
        let whole = ImageRange {
            aspects: Aspects::COLOR,
            base_level: 0,
            levels: 1,
            base_layer: 0,
            layers: 1,
        };
        commands.clear_image(image, ClearValue::Float(GROUND), &[whole])?;
        let layout = &self.layout;
        if let Some((top, rows)) = layout.band {
            let band = BufferImageCopy {
                buffer_offset: 0,
                bytes_per_row: layout.extent.0 * 4,
                rows_per_image: rows,
                image: COLOUR,
                origin: origin(0, top),
                extent: extent3d(layout.extent.0, rows),
            };
            commands.copy_buffer_to_image(&self.source, image, &[band])?;
        }
        let tiles: Vec<BufferImageCopy> = layout
            .tiles
            .iter()
            .map(|&(x, y)| BufferImageCopy {
                buffer_offset: layout.tile_offset(),
                bytes_per_row: TILE * 4,
                rows_per_image: TILE,
                image: COLOUR,
                origin: origin(x, y),
                extent: extent3d(TILE, TILE),
            })
            .collect();
        if !tiles.is_empty() {
            commands.copy_buffer_to_image(&self.source, image, &tiles)?;
        }
        if let (Some(&(x, y)), Some((to_x, to_y))) = (layout.tiles.first(), layout.copy) {
            let copied = ImageCopy {
                src: COLOUR,
                src_origin: origin(x, y),
                dst: COLOUR,
                dst_origin: origin(to_x, to_y),
                extent: extent3d(TILE, TILE),
            };
            commands.copy_image(image, image, &[copied])?;
        }
        Ok(())
    }
}

/// The memory a software device drawing the scene at `extent` is made with:
/// a picture, the staging it is copied through and the scene's own bytes,
/// twice over across a resize, held in the half of it its heap offers.
#[must_use]
pub fn memory_for((width, height): (u32, u32)) -> u64 {
    let frame = u64::from(width) * u64::from(height) * 4;
    frame.saturating_mul(8).max(REFERENCE_MEMORY)
}

/// The scene at `extent` in `format`, drawn on a software device made with
/// `clock` and read back: its rows packed, as a window's frame of that format
/// holds them.
///
/// # Errors
///
/// [`Error::TooLarge`] or [`Error::OutOfMemory`] where the host cannot hold
/// the picture, refused before the device draws it; otherwise what the
/// software device refused, or the format, as [`Scene::new`].
pub fn rendered(
    extent: (u32, u32),
    format: Format,
    clock: Arc<dyn MonotonicClock + Send + Sync>,
) -> Result<Vec<u8>, Error> {
    let frame = u64::from(extent.0) * u64::from(extent.1) * 4;
    let len = usize::try_from(frame).map_err(|_| Error::TooLarge)?;
    let mut bytes = room_for(len)?;
    bytes.resize(len, 0);
    let device = software(memory_for(extent), clock)?;
    let context = device.create_context(&ContextDesc::default())?;
    let desc = ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: extent3d(extent.0, extent.1),
        mip_levels: 1,
        array_layers: 1,
        samples: 1,
        usage: ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST),
    };
    let required = context.image_requirements(&desc)?;
    let picture_memory = context.allocate(&AllocationDesc {
        size: required.size,
        memory_type: required.memory_types.trailing_zeros(),
    })?;
    let picture = context.create_image(&picture_memory, 0, &desc)?;
    let near = near(&device)?;
    let readback_memory = context.allocate(&AllocationDesc {
        size: frame,
        memory_type: near,
    })?;
    let readback = context.create_buffer(
        &readback_memory,
        &BufferDesc {
            offset: 0,
            size: frame,
            usage: BufferUsage::TRANSFER_DST,
        },
    )?;
    let scene = Scene::new(&device, &context, extent, format)?;
    let mut commands = context.command_buffer();
    scene.record(&mut commands, &picture)?;
    commands.copy_image_to_buffer(
        &picture,
        &readback,
        &[BufferImageCopy {
            buffer_offset: 0,
            bytes_per_row: extent.0 * 4,
            rows_per_image: extent.1,
            image: COLOUR,
            origin: origin(0, 0),
            extent: extent3d(extent.0, extent.1),
        }],
    )?;
    submit(&context, clearing(&device)?, &commands)?;
    readback_memory.read(0, &mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
#[path = "scene_tests.rs"]
mod tests;
