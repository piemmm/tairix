//! The GICv3 interrupt translation service (Arm IHI 0069H chapter 5).
//!
//! A device raises an MSI by writing an `EventID` to `GITS_TRANSLATER`; the
//! fabric attaches the `DeviceID`, which the device cannot choose. The service
//! raises the LPI the kernel mapped for that pair, at the redistributor its
//! collection names, and drops anything else: an event no mapping names, or a
//! device with no mapping at all.
//!
//! It reads its command queue and tables from memory: a device table, a
//! collection table where its own collections run short, and an interrupt
//! translation table (ITT) per device.

use tairix_arch_api::PageTableFrames;

use crate::gicv3::{FIRST_LPI, TABLE_INNER_SHAREABLE, TABLE_SHAREABILITY};

const GITS_CTLR: usize = 0x0000;
const GITS_TYPER: usize = 0x0008;
const GITS_CBASER: usize = 0x0080;
const GITS_CWRITER: usize = 0x0088;
const GITS_CREADR: usize = 0x0090;
const GITS_BASER: usize = 0x0100;
const GITS_BASERS: usize = 8;
const PIDR2: usize = 0xFFE8;

/// `GITS_TRANSLATER`, a device's doorbell, in the translation frame.
pub const TRANSLATER: u64 = 0x1_0040;
/// The page of the translation frame `GITS_TRANSLATER` lies in: all a
/// device's domain maps, so it reaches no control register.
pub const TRANSLATION_PAGE: u64 = 0x1_0000;
/// The service's register frames, control then translation.
pub const FRAMES_BYTES: u64 = 0x2_0000;

const CTLR_ENABLED: u32 = 1 << 0;
const CTLR_QUIESCENT: u32 = 1 << 31;

const TYPER_PHYSICAL: u64 = 1 << 0;
const TYPER_PTA: u64 = 1 << 19;
const TYPER_CIL: u64 = 1 << 36;

const BASER_VALID: u64 = 1 << 63;
const BASER_INDIRECT: u64 = 1 << 62;
/// Inner cacheability of the command queue and the tables: Normal memory,
/// write-back, read- and write-allocate.
const INNER_WRITE_BACK: u64 = 0b111 << 59;
/// The same, non-cacheable.
const INNER_NON_CACHEABLE: u64 = 0b001 << 59;
const BASER_TYPE_SHIFT: u32 = 56;
const BASER_ENTRY_SHIFT: u32 = 48;
const BASER_PAGE_SIZE_SHIFT: u32 = 8;
const BASER_PAGE_SIZE: u64 = 0b11 << BASER_PAGE_SIZE_SHIFT;
/// A table's physical address, its bits `[47:12]`.
const BASER_PHYS: u64 = 0x0000_FFFF_FFFF_F000;
/// The command queue's physical address, its bits `[51:12]`.
const CBASER_PHYS: u64 = 0x000F_FFFF_FFFF_F000;
const TYPE_DEVICES: u64 = 1;
const TYPE_COLLECTIONS: u64 = 4;
/// A table's size field: pages, less one.
const MAX_TABLE_PAGES: u64 = 256;

/// Where the service reads or the kernel writes next: bits `[19:5]`.
const QUEUE_OFFSET: u64 = 0x000F_FFE0;
const CREADR_STALLED: u64 = 1 << 0;

const COMMAND_BYTES: u64 = 32;
/// The command queue holds `2^QUEUE_ORDER` frames: 64 KiB, 2048 commands.
const QUEUE_ORDER: u32 = 4;
const FRAME_BYTES: u64 = 4096;
/// An ITT's alignment.
const ITT_ALIGN: u64 = 256;

/// The most polls of a handshake before the service is declared
/// unresponsive: generous, each completing in microseconds on working
/// hardware.
const WAIT_SPINS: u32 = 1_000_000;

const CMD_SYNC: u64 = 0x05;
const CMD_MAPD: u64 = 0x08;
const CMD_MAPC: u64 = 0x09;
const CMD_MAPTI: u64 = 0x0A;

/// Volatile access to one service's control frame.
pub trait ItsMmio {
    /// Read the 32-bit register at `off`.
    fn read(&self, off: usize) -> u32;
    /// Read the 64-bit register at `off`.
    fn read_u64(&self, off: usize) -> u64;
    /// Write the 32-bit register at `off`.
    fn write(&self, off: usize, value: u32);
    /// Write the 64-bit register at `off`.
    fn write_u64(&self, off: usize, value: u64);
}

/// Why a service could not be taken over or asked something of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ItsError {
    /// Not a GICv3 or GICv4 ITS, or one translating to no physical LPIs.
    NotAnIts,
    /// A handshake or a command never completed.
    Unresponsive,
    /// The queue stalled on a command the service refused.
    Stalled,
    /// The service has no table of a kind it needs, or takes no page size
    /// the kernel can give one.
    NoTable,
    /// No memory for a table.
    Exhausted,
    /// A `DeviceID`, `EventID`, LPI or collection the service cannot name, a
    /// route given twice, or memory it cannot reach.
    OutOfRange,
    /// A device the service already translates.
    Mapped,
}

impl ItsError {
    /// The stable name an audit record gives the refusal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotAnIts => "its_not_an_its",
            Self::Unresponsive => "its_unresponsive",
            Self::Stalled => "its_stalled",
            Self::NoTable => "its_no_table",
            Self::Exhausted => "its_exhausted",
            Self::OutOfRange => "its_out_of_range",
            Self::Mapped => "its_mapped",
        }
    }
}

/// What a service offers (`GITS_TYPER`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ItsFeatures {
    /// `DeviceID` bits.
    pub device_bits: u32,
    /// `EventID` bits.
    pub event_bits: u32,
    /// Bytes in one ITT entry.
    pub itt_entry_bytes: u32,
    /// A collection names its redistributor by physical address, rather
    /// than by processor number.
    pub physical_targets: bool,
    /// Collections it holds itself, with no table.
    pub hardware_collections: u32,
    /// Collection id bits.
    pub collection_bits: u32,
}

/// One device's event the kernel maps: `device`'s messages carrying `event`
/// raise LPI `lpi`, an INTID from [`FIRST_LPI`] up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ItsRoute {
    /// The `DeviceID` the fabric attaches to its messages.
    pub device: u32,
    /// The `EventID` it writes.
    pub event: u32,
    /// The LPI raised.
    pub lpi: u32,
}

/// One 32-byte command.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Command([u64; 4]);

impl Command {
    /// `MAPD`: `device` translates through the ITT at `itt` of
    /// `2^event_bits` entries.
    #[must_use]
    pub const fn map_device(device: u32, event_bits: u32, itt: u64) -> Self {
        Self([
            CMD_MAPD | ((device as u64) << 32),
            (event_bits.saturating_sub(1) as u64) & 0x1F,
            BASER_VALID | (itt & 0x000F_FFFF_FFFF_FF00),
            0,
        ])
    }

    /// `MAPC`: collection `icid` raises its LPIs at `target`, a
    /// redistributor as `crate::gicv3::Gicv3::collection_target` names it.
    #[must_use]
    pub const fn map_collection(icid: u16, target: u64) -> Self {
        Self([
            CMD_MAPC,
            0,
            BASER_VALID | (target & 0x000F_FFFF_FFFF_0000) | icid as u64,
            0,
        ])
    }

    /// `MAPTI`: `device`'s `event` raises `lpi` in collection `icid`.
    #[must_use]
    pub const fn map_event(device: u32, event: u32, lpi: u32, icid: u16) -> Self {
        Self([
            CMD_MAPTI | ((device as u64) << 32),
            event as u64 | ((lpi as u64) << 32),
            icid as u64,
            0,
        ])
    }

    /// `SYNC`: every earlier command's effect at `target` is visible.
    #[must_use]
    pub const fn sync(target: u64) -> Self {
        Self([CMD_SYNC, 0, target & 0x000F_FFFF_FFFF_0000, 0])
    }

    /// The command's words, as the service reads them.
    #[must_use]
    pub const fn words(self) -> [u64; 4] {
        self.0
    }
}

/// A service not yet taken over.
pub struct Its<M> {
    mmio: M,
}

impl<M: ItsMmio> Its<M> {
    /// Bind a driver to a service's control frame.
    pub const fn new(mmio: M) -> Self {
        Self { mmio }
    }

    /// What the service offers.
    ///
    /// # Errors
    ///
    /// [`ItsError::NotAnIts`] for a frame of another revision, or a service
    /// translating to no physical LPIs.
    pub fn features(&self) -> Result<ItsFeatures, ItsError> {
        if !matches!((self.mmio.read(PIDR2) >> 4) & 0xF, 3 | 4) {
            return Err(ItsError::NotAnIts);
        }
        let typer = self.mmio.read_u64(GITS_TYPER);
        if typer & TYPER_PHYSICAL == 0 {
            return Err(ItsError::NotAnIts);
        }
        let field = |shift: u32, width: u32| {
            u32::try_from((typer >> shift) & ((1 << width) - 1)).unwrap_or(u32::MAX)
        };
        Ok(ItsFeatures {
            itt_entry_bytes: field(4, 4) + 1,
            event_bits: field(8, 5) + 1,
            device_bits: field(13, 5) + 1,
            physical_targets: typer & TYPER_PTA != 0,
            hardware_collections: field(24, 8),
            collection_bits: if typer & TYPER_CIL != 0 {
                field(32, 4) + 1
            } else {
                16
            },
        })
    }

    /// Take the service over for `collections` collections: quiesce it if
    /// firmware left it on, give it a command queue and the tables it needs
    /// from `frames`, and enable it. A failure leaves it off, its memory given
    /// back.
    ///
    /// # Errors
    ///
    /// [`ItsError::NotAnIts`], [`ItsError::Unresponsive`] for one that never
    /// quiesces, [`ItsError::NoTable`], [`ItsError::Exhausted`], or
    /// [`ItsError::OutOfRange`] for memory it cannot reach.
    pub fn take_over(
        self,
        frames: &dyn PageTableFrames,
        collections: u32,
    ) -> Result<ItsUnit<'_, M>, ItsError> {
        let features = self.features()?;
        let ctlr = self.mmio.read(GITS_CTLR);
        if ctlr & CTLR_ENABLED != 0 {
            self.mmio.write(GITS_CTLR, ctlr & !CTLR_ENABLED);
        }
        wait(|| Ok(self.mmio.read(GITS_CTLR) & CTLR_QUIESCENT != 0))?;
        let mut owned = Owned {
            frames,
            blocks: [(0, 0); 3],
            count: 0,
        };
        let queue = self.queue(&mut owned)?;
        let mut devices = None;
        let mut held = features.hardware_collections;
        for n in 0..GITS_BASERS {
            let off = GITS_BASER + 8 * n;
            let baser = self.mmio.read_u64(off);
            match (baser >> BASER_TYPE_SHIFT) & 0b111 {
                TYPE_DEVICES if devices.is_none() => {
                    let ids = 1u64 << features.device_bits;
                    devices = Some(self.table(off, ids, true, &mut owned)?);
                }
                // Collections are few, so their table is one flat page.
                TYPE_COLLECTIONS if held < collections => {
                    let table = self.table(off, u64::from(collections), false, &mut owned)?;
                    held = u32::try_from(table.capacity).unwrap_or(u32::MAX);
                }
                _ => self.mmio.write_u64(off, baser & !BASER_VALID),
            }
        }
        let devices = devices.ok_or(ItsError::NoTable)?;
        if held < collections {
            return Err(ItsError::NoTable);
        }
        owned.keep();
        let ctlr = self.mmio.read(GITS_CTLR);
        self.mmio.write(GITS_CTLR, ctlr | CTLR_ENABLED);
        Ok(ItsUnit {
            mmio: self.mmio,
            features,
            frames,
            queue,
            devices,
            collections: held.min(1 << features.collection_bits.min(16)),
            mapped: false,
        })
    }

    /// Give the service its command queue, empty.
    fn queue(&self, owned: &mut Owned<'_>) -> Result<Queue, ItsError> {
        let phys = owned.block(QUEUE_ORDER, CBASER_PHYS)?;
        let pages = (1u64 << QUEUE_ORDER) - 1;
        let base = BASER_VALID | (phys & CBASER_PHYS) | pages;
        let coherent = self.program(GITS_CBASER, base);
        self.mmio.write_u64(GITS_CWRITER, 0);
        Ok(Queue {
            phys,
            write: 0,
            batch: 0,
            coherent,
        })
    }

    /// Give the table `GITS_BASER` at `off` names memory for `ids` entries:
    /// flat where one page holds them or `two_level` is refused, or the
    /// service walks no two-level table; two-level otherwise, its leaf pages
    /// drawn as their entries are first used. At the smallest page size the
    /// service takes, and at most the pages its size field counts.
    fn table(
        &self,
        off: usize,
        ids: u64,
        two_level: bool,
        owned: &mut Owned<'_>,
    ) -> Result<Table, ItsError> {
        let baser = self.mmio.read_u64(off);
        let entry_bytes = ((baser >> BASER_ENTRY_SHIFT) & 0x1F) + 1;
        let page_order = self.page_order(off, baser)?;
        let page_bytes = FRAME_BYTES << page_order;
        let fixed = baser & ((0b111 << BASER_TYPE_SHIFT) | (0x1F << BASER_ENTRY_SHIFT));
        let page_code = page_size_code(page_order);
        let per_page = page_bytes / entry_bytes;
        let indirect = two_level && ids > per_page && {
            self.mmio.write_u64(off, fixed | BASER_INDIRECT | page_code);
            self.mmio.read_u64(off) & BASER_INDIRECT != 0
        };
        // Each first-level entry is eight bytes naming one leaf page.
        let per_unit = if indirect {
            per_page * (page_bytes / 8)
        } else {
            per_page
        };
        let pages = ids.div_ceil(per_unit).clamp(1, MAX_TABLE_PAGES);
        let capacity = (pages * per_unit).min(ids);
        let order = order_of(pages * page_bytes);
        let phys = owned.block(order, BASER_PHYS)?;
        let value = fixed
            | BASER_VALID
            | if indirect { BASER_INDIRECT } else { 0 }
            | (phys & BASER_PHYS)
            | page_code
            | (pages - 1);
        let coherent = self.program(off, value);
        Ok(Table {
            phys,
            order,
            page_order,
            entry_bytes,
            indirect,
            coherent,
            capacity,
        })
    }

    /// The smallest page size the table at `off` takes, as an order of
    /// frames.
    fn page_order(&self, off: usize, baser: u64) -> Result<u32, ItsError> {
        let fixed = baser & !BASER_VALID & !BASER_PAGE_SIZE;
        [0, 2, 4]
            .into_iter()
            .find(|&order| {
                let code = page_size_code(order);
                self.mmio.write_u64(off, fixed | code);
                self.mmio.read_u64(off) & BASER_PAGE_SIZE == code
            })
            .ok_or(ItsError::NoTable)
    }

    /// Write the queue or table register at `off` cacheable and inner
    /// shareable, falling back to non-cacheable where the service shares
    /// nothing, and answer whether it shares.
    fn program(&self, off: usize, value: u64) -> bool {
        self.mmio
            .write_u64(off, value | INNER_WRITE_BACK | TABLE_INNER_SHAREABLE);
        if self.mmio.read_u64(off) & TABLE_SHAREABILITY != 0 {
            return true;
        }
        self.mmio.write_u64(off, value | INNER_NON_CACHEABLE);
        false
    }
}

/// The `GITS_BASER` page-size field for pages of `2^order` frames.
const fn page_size_code(order: u32) -> u64 {
    let code = match order {
        0 => 0b00,
        2 => 0b01,
        _ => 0b10,
    };
    code << BASER_PAGE_SIZE_SHIFT
}

/// The `EventID` bits an ITT holding every event of `device` up to its
/// highest takes: at least one, the smallest the service maps. `device` is
/// one device's routes, sorted by event.
fn event_bits(device: &[ItsRoute]) -> u32 {
    let highest = device.last().map_or(0, |route| route.event);
    (u32::BITS - highest.leading_zeros()).max(1)
}

/// The order of the smallest block of frames holding `bytes`.
#[must_use]
pub fn order_of(bytes: u64) -> u32 {
    bytes
        .div_ceil(FRAME_BYTES)
        .next_power_of_two()
        .trailing_zeros()
}

fn wait(mut done: impl FnMut() -> Result<bool, ItsError>) -> Result<(), ItsError> {
    for _ in 0..WAIT_SPINS {
        if done()? {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(ItsError::Unresponsive)
}

/// Blocks a take-over has drawn — its queue and at most two tables — given
/// back unless it succeeds.
struct Owned<'f> {
    frames: &'f dyn PageTableFrames,
    blocks: [(u64, u32); 3],
    count: usize,
}

impl Owned<'_> {
    /// A zeroed block of `2^order` frames whose physical address `reach`
    /// covers, its zeroes cleaned to the point of coherency so a service
    /// that does not snoop reads them.
    fn block(&mut self, order: u32, reach: u64) -> Result<u64, ItsError> {
        let slot = self.blocks.get_mut(self.count).ok_or(ItsError::Exhausted)?;
        let phys = self.frames.alloc_block(order).ok_or(ItsError::Exhausted)?;
        *slot = (phys, order);
        self.count += 1;
        if phys & !reach != 0 {
            return Err(ItsError::OutOfRange);
        }
        let base = self
            .frames
            .block_at(phys, order)
            .ok_or(ItsError::OutOfRange)?;
        crate::paging::clean_range_to_poc(base as u64, FRAME_BYTES << order);
        Ok(phys)
    }

    /// Keep every block, now the service reads them for the kernel's life.
    fn keep(mut self) {
        self.count = 0;
    }
}

impl Drop for Owned<'_> {
    fn drop(&mut self) {
        for &(phys, order) in &self.blocks[..self.count] {
            self.frames.free_block(phys, order);
        }
    }
}

/// The command queue: where the next command goes, and where the batch not
/// yet handed over starts.
struct Queue {
    phys: u64,
    write: u64,
    batch: u64,
    coherent: bool,
}

/// A table the service reads: flat, or a first level of leaf pages.
struct Table {
    phys: u64,
    order: u32,
    page_order: u32,
    entry_bytes: u64,
    indirect: bool,
    coherent: bool,
    /// How many ids it holds entries for.
    capacity: u64,
}

/// A service taken over: its registers, and the memory it reads for the
/// kernel's life.
pub struct ItsUnit<'f, M> {
    mmio: M,
    features: ItsFeatures,
    frames: &'f dyn PageTableFrames,
    queue: Queue,
    devices: Table,
    /// Collection ids it holds, from zero.
    collections: u32,
    /// Its devices are mapped: they all are at once.
    mapped: bool,
}

impl<M: ItsMmio> ItsUnit<'_, M> {
    /// What the service offers.
    #[must_use]
    pub fn features(&self) -> ItsFeatures {
        self.features
    }

    /// Map collection `icid` to the redistributor `target` names, then
    /// every route in it, each device given an ITT for every event up to its
    /// highest, the effects visible at `target` before this returns. A
    /// service's devices are all mapped by one call, before any of them can
    /// master; nothing reaches the service unless every route is one it can
    /// name. `routes` is sorted in place.
    ///
    /// # Errors
    ///
    /// [`ItsError::OutOfRange`] for a route naming a `DeviceID`, `EventID` or
    /// LPI past what the service and `id_bits` cover, a route given twice,
    /// or a collection past its own; [`ItsError::Mapped`] once its devices
    /// are; [`ItsError::Exhausted`]; or the queue's [`ItsError::Stalled`] or
    /// [`ItsError::Unresponsive`].
    pub fn map(
        &mut self,
        icid: u16,
        target: u64,
        id_bits: u32,
        routes: &mut [ItsRoute],
    ) -> Result<(), ItsError> {
        if self.mapped {
            return Err(ItsError::Mapped);
        }
        let lpis = FIRST_LPI..1u32.checked_shl(id_bits).ok_or(ItsError::OutOfRange)?;
        let devices = (1u64 << self.features.device_bits).min(self.devices.capacity);
        let events = 1u64 << self.features.event_bits;
        let in_range = |route: &ItsRoute| {
            u64::from(route.device) < devices
                && u64::from(route.event) < events
                && lpis.contains(&route.lpi)
        };
        if u32::from(icid) >= self.collections || !routes.iter().all(in_range) {
            return Err(ItsError::OutOfRange);
        }
        routes.sort_unstable_by_key(|route| (route.device, route.event));
        if routes
            .windows(2)
            .any(|pair| (pair[0].device, pair[0].event) == (pair[1].device, pair[1].event))
        {
            return Err(ItsError::OutOfRange);
        }
        // Every allocation is made before a command is written, so running
        // out leaves the service as it was.
        let mut itt_bytes = 0u64;
        for device in routes.chunk_by(|a, b| a.device == b.device) {
            self.leaf(device[0].device)?;
            itt_bytes += self.itt_bytes(event_bits(device));
        }
        let mut itt = if itt_bytes == 0 {
            0
        } else {
            self.block(order_of(itt_bytes))?
        };
        self.mapped = true;
        self.emit(Command::map_collection(icid, target))?;
        for device in routes.chunk_by(|a, b| a.device == b.device) {
            let bits = event_bits(device);
            self.emit(Command::map_device(device[0].device, bits, itt))?;
            itt += self.itt_bytes(bits);
            for route in device {
                self.emit(Command::map_event(
                    route.device,
                    route.event,
                    route.lpi,
                    icid,
                ))?;
            }
        }
        self.emit(Command::sync(target))?;
        self.flush()
    }

    /// Bytes an ITT of `2^event_bits` entries takes, rounded to the
    /// alignment the next one needs.
    fn itt_bytes(&self, event_bits: u32) -> u64 {
        (u64::from(self.features.itt_entry_bytes) << event_bits).next_multiple_of(ITT_ALIGN)
    }

    /// Give a two-level device table the leaf page holding `device`'s entry.
    fn leaf(&mut self, device: u32) -> Result<(), ItsError> {
        if !self.devices.indirect {
            return Ok(());
        }
        let per_page = (FRAME_BYTES << self.devices.page_order) / self.devices.entry_bytes;
        let index =
            usize::try_from(u64::from(device) / per_page).map_err(|_| ItsError::OutOfRange)?;
        let level = self
            .frames
            .block_at(self.devices.phys, self.devices.order)
            .ok_or(ItsError::OutOfRange)?;
        // SAFETY: `index` names an entry of the first level, which the
        // take-over sized to hold a leaf for every DeviceID the capacity
        // admits, and `map` admitted `device`; the service reads the level
        // but only the kernel writes it.
        let entry = unsafe { level.add(index) };
        // SAFETY: as above.
        if unsafe { entry.read_volatile() } & BASER_VALID != 0 {
            return Ok(());
        }
        let leaf = self.block(self.devices.page_order)?;
        if leaf & !BASER_PHYS != 0 {
            self.frames.free_block(leaf, self.devices.page_order);
            return Err(ItsError::OutOfRange);
        }
        // SAFETY: as above.
        unsafe { entry.write_volatile(BASER_VALID | leaf) };
        Self::publish(entry as u64, 8, self.devices.coherent);
        Ok(())
    }

    /// A zeroed block of `2^order` frames the service may read, its zeroes
    /// at the point of coherency, kept for the kernel's life.
    fn block(&mut self, order: u32) -> Result<u64, ItsError> {
        let phys = self.frames.alloc_block(order).ok_or(ItsError::Exhausted)?;
        let Some(base) = self.frames.block_at(phys, order) else {
            self.frames.free_block(phys, order);
            return Err(ItsError::OutOfRange);
        };
        crate::paging::clean_range_to_poc(base as u64, FRAME_BYTES << order);
        Ok(phys)
    }

    /// Write `command` at the queue's end, handing the batch over first
    /// where the queue has no room left.
    fn emit(&mut self, command: Command) -> Result<(), ItsError> {
        let queue_bytes = FRAME_BYTES << QUEUE_ORDER;
        if (self.queue.write + COMMAND_BYTES) % queue_bytes == self.queue.batch {
            self.flush()?;
        }
        let base = self
            .frames
            .block_at(self.queue.phys, QUEUE_ORDER)
            .ok_or(ItsError::OutOfRange)?;
        let word = usize::try_from(self.queue.write / 8).map_err(|_| ItsError::OutOfRange)?;
        for (n, value) in command.words().into_iter().enumerate() {
            // SAFETY: `word + n` lies in the queue block; the service reads
            // it only up to `GITS_CWRITER`, which has not reached it.
            unsafe { base.add(word + n).write_volatile(value) };
        }
        self.queue.write = (self.queue.write + COMMAND_BYTES) % queue_bytes;
        Ok(())
    }

    /// Hand the service the commands written since the last hand-over and
    /// wait for it to consume them.
    fn flush(&mut self) -> Result<(), ItsError> {
        let queue_bytes = FRAME_BYTES << QUEUE_ORDER;
        let base = self
            .frames
            .block_at(self.queue.phys, QUEUE_ORDER)
            .ok_or(ItsError::OutOfRange)? as u64;
        let (first, written) = (self.queue.batch, self.queue.write);
        if written >= first {
            Self::publish(base + first, written - first, self.queue.coherent);
        } else {
            Self::publish(base + first, queue_bytes - first, self.queue.coherent);
            Self::publish(base, written, self.queue.coherent);
        }
        self.mmio.write_u64(GITS_CWRITER, written);
        self.queue.batch = written;
        wait(|| {
            let creadr = self.mmio.read_u64(GITS_CREADR);
            if creadr & CREADR_STALLED != 0 {
                return Err(ItsError::Stalled);
            }
            Ok(creadr & QUEUE_OFFSET == written)
        })
    }

    /// Make `len` bytes the CPU wrote at `base` visible to the service: a
    /// clean to the point of coherency where it shares nothing, else a
    /// barrier ordering the stores before the register write that hands
    /// them over.
    fn publish(base: u64, len: u64, coherent: bool) {
        if coherent {
            store_barrier();
        } else {
            crate::paging::clean_range_to_poc(base, len);
        }
    }
}

/// Order this CPU's earlier stores to Normal memory before its next
/// register write, inner shareable domain.
fn store_barrier() {
    #[cfg(all(target_arch = "aarch64", target_os = "none"))]
    // SAFETY: a barrier only; it touches no memory.
    unsafe {
        core::arch::asm!("dsb ishst", options(nostack, preserves_flags));
    }
}

/// Bare-metal [`ItsMmio`] over a service's control frame at `base`, which
/// the boot maps as Device memory.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub struct VolatileItsMmio {
    base: usize,
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
impl VolatileItsMmio {
    /// The control frame at `base`.
    ///
    /// # Safety
    ///
    /// `base` is a service's control frame, mapped as Device memory for the
    /// kernel's life, and nothing else drives it.
    #[must_use]
    pub const unsafe fn new(base: usize) -> Self {
        Self { base }
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
impl ItsMmio for VolatileItsMmio {
    fn read(&self, off: usize) -> u32 {
        // SAFETY: `off` names a register of the frame `new`'s contract maps.
        unsafe { core::ptr::read_volatile((self.base + off) as *const u32) }
    }
    fn read_u64(&self, off: usize) -> u64 {
        // SAFETY: as `read`; the 64-bit registers take a 64-bit load.
        unsafe { core::ptr::read_volatile((self.base + off) as *const u64) }
    }
    fn write(&self, off: usize, value: u32) {
        // SAFETY: as `read`.
        unsafe { core::ptr::write_volatile((self.base + off) as *mut u32, value) }
    }
    fn write_u64(&self, off: usize, value: u64) {
        // SAFETY: as `read_u64`.
        unsafe { core::ptr::write_volatile((self.base + off) as *mut u64, value) }
    }
}

#[cfg(test)]
#[path = "its_tests.rs"]
mod tests;
