//! The logical volume's partitions: how a logical block address becomes a
//! sector of the device (ECMA-167 3/10.7, OSTA UDF 2.60 §2.2.8–2.2.13).

use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_fsprobe::udf::{Partition, ShortAd};

/// A logical block address: a block of a partition, by its reference.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct LbAddr {
    pub block: u32,
    pub partition: u16,
}

/// Most entries one sparing table may hold: a validation bound on untrusted
/// media, far beyond any drive's spare area.
pub(crate) const MAX_SPARING_ENTRIES: usize = 4096;

/// Most entries a virtual allocation table may hold: one per block a
/// write-once disc of the largest kind records.
pub(crate) const MAX_VAT_ENTRIES: usize = 1 << 27;

/// A physical partition: a run of sectors.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Physical {
    pub number: u16,
    pub start: u32,
    pub length: u32,
    pub access: u32,
    pub bitmap: ShortAd,
    pub table: ShortAd,
}

impl Physical {
    pub(crate) fn of(partition: &Partition) -> Self {
        Self {
            number: partition.number,
            start: partition.start,
            length: partition.length,
            access: partition.access,
            bitmap: partition.unallocated_bitmap,
            table: partition.unallocated_table,
        }
    }

    fn sector(&self, block: u32) -> Result<u64, DriverError> {
        if block >= self.length {
            return Err(DriverError::DeviceFault);
        }
        Ok(u64::from(self.start) + u64::from(block))
    }
}

/// A run of blocks of the partition beneath a metadata partition: one
/// extent of its metadata file, and the block of the file it starts.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Run {
    pub first: u32,
    pub block: u32,
    pub blocks: u32,
}

/// One partition map, resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Map {
    Physical(Physical),
    /// A physical partition whose defective packets are remapped.
    Sparable {
        physical: Physical,
        /// Blocks per packet, never zero.
        packet: u32,
        /// `(original, mapped)`: the packet's first block, partition
        /// relative, and the sector it now lives at; sorted by original,
        /// one entry each.
        table: Vec<(u32, u32)>,
    },
    /// A write-once partition whose blocks the virtual allocation table
    /// places in the partition map `beneath`.
    Virtual {
        beneath: usize,
        table: Vec<u32>,
    },
    /// Metadata kept as the content of a file in the partition map
    /// `beneath`: its main file, or its mirror when the main cannot be read,
    /// its runs in the order the file's blocks follow.
    Metadata {
        beneath: usize,
        file: Vec<Run>,
    },
}

/// The volume's partition maps, by partition reference number.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Partitions {
    pub maps: Vec<Map>,
}

impl Partitions {
    /// The sector logical block `addr` lies in.
    pub(crate) fn sector(&self, addr: LbAddr) -> Result<u64, DriverError> {
        self.sector_of(addr, 0)
    }

    fn sector_of(&self, addr: LbAddr, depth: u8) -> Result<u64, DriverError> {
        // A map may sit on one other, never on a chain.
        if depth > 2 {
            return Err(DriverError::DeviceFault);
        }
        let map = self
            .maps
            .get(usize::from(addr.partition))
            .ok_or(DriverError::DeviceFault)?;
        match map {
            Map::Physical(physical) => physical.sector(addr.block),
            Map::Sparable {
                physical,
                packet,
                table,
            } => {
                physical.sector(addr.block)?;
                match remapped(table, addr.block - addr.block % packet) {
                    Some(mapped) => Ok(u64::from(mapped) + u64::from(addr.block % packet)),
                    None => physical.sector(addr.block),
                }
            }
            Map::Virtual { beneath, table } => {
                let physical = *table
                    .get(usize::try_from(addr.block).map_err(|_| DriverError::DeviceFault)?)
                    .ok_or(DriverError::DeviceFault)?;
                if physical == u32::MAX {
                    return Err(DriverError::DeviceFault);
                }
                self.sector_of(beneath_addr(*beneath, physical)?, depth + 1)
            }
            Map::Metadata { beneath, file } => {
                let block = run_block(file, addr.block)?;
                self.sector_of(beneath_addr(*beneath, block)?, depth + 1)
            }
        }
    }

    /// How many blocks from `addr` lie in consecutive sectors, up to `most`:
    /// what one device read may cover.
    pub(crate) fn contiguous(&self, addr: LbAddr, most: u32) -> Result<u32, DriverError> {
        match self.maps.get(usize::from(addr.partition)) {
            Some(Map::Physical(physical)) => {
                physical.sector(addr.block)?;
                return Ok(most.min(physical.length - addr.block));
            }
            Some(Map::Sparable {
                physical,
                packet,
                table,
            }) => {
                physical.sector(addr.block)?;
                let packet_start = addr.block - addr.block % packet;
                if remapped(table, packet_start).is_some() {
                    return Ok(most.min(packet - addr.block % packet));
                }
                // As laid out up to the next packet remapped, if any is.
                let after = table.partition_point(|&(original, _)| original <= packet_start);
                let end = table.get(after).map_or(physical.length, |&(original, _)| {
                    original.min(physical.length)
                });
                return Ok(most.min(end.saturating_sub(addr.block).max(1)));
            }
            _ => {}
        }
        let first = self.sector(addr)?;
        let mut run = 1;
        while run < most {
            let Some(block) = addr.block.checked_add(run) else {
                break;
            };
            match self.sector(LbAddr {
                block,
                partition: addr.partition,
            }) {
                Ok(sector) if sector == first + u64::from(run) => run += 1,
                _ => break,
            }
        }
        Ok(run)
    }

    /// The physical partition beneath `partition`, through the maps it sits
    /// on: at most a metadata partition on a virtual one on a physical one,
    /// so a longer chain — or a cycle — is none.
    pub(crate) fn physical(&self, partition: u16) -> Option<&Physical> {
        let mut at = usize::from(partition);
        for _ in 0..3 {
            match self.maps.get(at)? {
                Map::Physical(physical) | Map::Sparable { physical, .. } => return Some(physical),
                Map::Virtual { beneath, .. } | Map::Metadata { beneath, .. } => at = *beneath,
            }
        }
        None
    }
}

/// The sector a sparing table moved the packet starting at block `first` to.
fn remapped(table: &[(u32, u32)], first: u32) -> Option<u32> {
    table
        .binary_search_by_key(&first, |&(original, _)| original)
        .ok()
        .map(|at| table[at].1)
}

/// The address of block `block` of the map `beneath`.
fn beneath_addr(beneath: usize, block: u32) -> Result<LbAddr, DriverError> {
    Ok(LbAddr {
        block,
        partition: u16::try_from(beneath).map_err(|_| DriverError::DeviceFault)?,
    })
}

/// The block of the partition beneath that block `index` of a file laid out
/// as `runs` lies in.
fn run_block(runs: &[Run], index: u32) -> Result<u32, DriverError> {
    let at = runs
        .partition_point(|run| run.first <= index)
        .checked_sub(1)
        .ok_or(DriverError::DeviceFault)?;
    let run = runs[at];
    let within = index - run.first;
    if within >= run.blocks {
        return Err(DriverError::DeviceFault);
    }
    run.block
        .checked_add(within)
        .ok_or(DriverError::DeviceFault)
}
