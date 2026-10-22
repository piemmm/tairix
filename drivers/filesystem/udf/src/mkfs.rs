//! Laying a new UDF volume out on a whole device: the recognition sequence,
//! anchors at sector 256 and the last sector, a main and a reserve volume
//! descriptor sequence, the integrity descriptor, and one overwritable
//! partition holding the space bitmap, the file set and an empty root.

use alloc::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_fsprobe::udf::{
    Tag, ACCESS_OVERWRITABLE, DOMAIN_ID, TAG_AVDP, TAG_EFE, TAG_FE, TAG_FID, TAG_FSD, TAG_IUVD,
    TAG_LEN, TAG_LVD, TAG_LVID, TAG_PD, TAG_PVD, TAG_SBD, TAG_TD, TAG_USD, VRS_START,
};

use crate::dir::{self, CHAR_DIRECTORY, CHAR_PARENT, FID_FIXED};
use crate::entry::{EFE_FIXED, FE_FIXED, STRATEGY_4, TYPE_DIRECTORY};
use crate::fields::encode_timestamp;
use crate::integrity::FIRST_UNIQUE_ID;
use crate::space::HEADER;
use crate::{zeroed_len, Udf, UNSPECIFIED_ID};

/// How a new volume is laid out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FormatOptions<'a> {
    /// The volume's label, kept as both the volume and the logical volume
    /// identifier.
    pub label: &'a str,
    /// The UDF revision to record: from 0x0102 to 0x0201, the newest a
    /// volume without a metadata partition may claim.
    pub revision: u16,
    /// Eight bytes the volume set identifier makes unique, as UDF asks:
    /// random, or the formatting instant.
    pub serial: [u8; 8],
    /// The instant the volume is recorded at.
    pub now: Time64,
}

/// The descriptor character set every identifier is recorded in.
const CS0: &[u8] = b"OSTA Compressed Unicode";

/// The implementation this formatter records itself as.
const IMPLEMENTATION: &[u8] = b"*TAIRiX UDF";

/// Sectors of each volume descriptor sequence.
const VDS_SECTORS: u64 = 16;

/// Where the anchor that opens a volume lies.
const ANCHOR: u64 = 256;

/// The newest revision a volume without a metadata partition may record.
const NEWEST: u16 = 0x0201;

/// Where things go on a device of `blocks` blocks of `bs` bytes.
struct Plan {
    bs: u64,
    main: u64,
    reserve: u64,
    integrity: u64,
    partition: u64,
    length: u64,
    bitmap_blocks: u64,
    file_set: u64,
    root: u64,
    root_data: u64,
}

impl Plan {
    fn of(blocks: u64, bs: u64) -> Result<Self, DriverError> {
        // The sequences start past the recognition sequence, before the
        // anchor at 256.
        let main = (64 * 1024u64).div_ceil(bs);
        let reserve = main + VDS_SECTORS;
        let integrity = reserve + VDS_SECTORS;
        if integrity + 2 > ANCHOR {
            return Err(DriverError::Unsupported);
        }
        let partition = ANCHOR + 1;
        let length = blocks
            .checked_sub(partition + 1)
            .filter(|&len| len >= 64)
            .ok_or(DriverError::NoSpace)?;
        let length = length.min(u64::from(u32::MAX));
        let bitmap_blocks = (HEADER + length.div_ceil(8)).div_ceil(bs);
        let file_set = bitmap_blocks;
        Ok(Self {
            bs,
            main,
            reserve,
            integrity,
            partition,
            length,
            bitmap_blocks,
            file_set,
            root: file_set + 2,
            root_data: file_set + 3,
        })
    }

    /// Blocks of the partition in use once the volume is laid out: the
    /// bitmap, the file set and its terminator, the root and its records.
    fn used(&self) -> u64 {
        self.root_data + 1
    }
}

fn u32_of(value: u64) -> Result<u32, DriverError> {
    u32::try_from(value).map_err(|_| DriverError::NoSpace)
}

/// A `dstring` of `len` bytes holding `text`.
fn dstring(text: &str, len: usize) -> Result<Vec<u8>, DriverError> {
    let mut field = zeroed_len(len)?;
    if text.is_empty() {
        return Ok(field);
    }
    let identifier = dir::encode(text).ok_or(DriverError::LengthOutOfRange)?;
    if identifier.len() >= len {
        return Err(DriverError::LengthOutOfRange);
    }
    field[..identifier.len()].copy_from_slice(&identifier);
    field[len - 1] = u8::try_from(identifier.len()).map_err(|_| DriverError::LengthOutOfRange)?;
    Ok(field)
}

/// A character set specification naming OSTA Compressed Unicode.
fn charspec(out: &mut [u8]) {
    out[0] = 0;
    out[1..=CS0.len()].copy_from_slice(CS0);
}

/// A regid of `identifier` with `suffix`.
fn regid(out: &mut [u8], identifier: &[u8], suffix: &[u8]) {
    out[1..=identifier.len()].copy_from_slice(identifier);
    out[24..24 + suffix.len()].copy_from_slice(suffix);
}

/// A domain identifier: UDF's domain, the revision and no protection.
fn domain(out: &mut [u8], revision: u16) {
    regid(out, DOMAIN_ID, &revision.to_le_bytes());
}

/// A descriptor of `len` bytes sealed as `id` at `location`.
fn sealed(
    mut bytes: Vec<u8>,
    id: u16,
    version: u16,
    location: u64,
    len: usize,
) -> Result<Vec<u8>, DriverError> {
    Tag {
        id,
        version,
        serial: 0,
        location: u32_of(location)?,
    }
    .seal(&mut bytes, len - TAG_LEN)?;
    Ok(bytes)
}

impl<B: Block> Udf<B> {
    /// Lay a new volume out over the whole of `device` and mount it.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] for a revision this formatter does
    ///   not lay out, or a block size too large for its layout.
    /// * [`DriverError::NoSpace`] for a device too small to hold a volume.
    /// * [`DriverError::LengthOutOfRange`] for a label no identifier holds.
    /// * Whatever the device reports for a write.
    pub fn format(device: B, options: &FormatOptions<'_>) -> Result<Self, DriverError> {
        if !(0x0102..=NEWEST).contains(&options.revision) {
            return Err(DriverError::Unsupported);
        }
        let mut dev = ByteDevice::new(device)?;
        let bs = u64::from(dev.block_size());
        let blocks = dev.block_count();
        let plan = Plan::of(blocks, bs)?;
        let version: u16 = if options.revision >= 0x0200 { 3 } else { 2 };
        let stamp = encode_timestamp(options.now).ok_or(DriverError::OutOfRange)?;
        // Nothing an earlier format left at either end may be read as this
        // volume's.
        dev.zero(0, (ANCHOR + 1) * bs)?;
        dev.zero((blocks - ANCHOR - 1) * bs, (ANCHOR + 1) * bs)?;
        recognition(&mut dev, version)?;
        for anchor in [ANCHOR, blocks - 1] {
            let mut avdp = zeroed_len(512)?;
            avdp[16..20].copy_from_slice(&u32_of(VDS_SECTORS * bs)?.to_le_bytes());
            avdp[20..24].copy_from_slice(&u32_of(plan.main)?.to_le_bytes());
            avdp[24..28].copy_from_slice(&u32_of(VDS_SECTORS * bs)?.to_le_bytes());
            avdp[28..32].copy_from_slice(&u32_of(plan.reserve)?.to_le_bytes());
            dev.write(anchor * bs, &sealed(avdp, TAG_AVDP, version, anchor, 512)?)?;
        }
        for start in [plan.main, plan.reserve] {
            let descriptors = descriptors(&plan, options, version, start, stamp)?;
            for (index, descriptor) in descriptors.iter().enumerate() {
                dev.write((start + index as u64) * bs, descriptor)?;
            }
        }
        dev.write(
            plan.integrity * bs,
            &integrity(&plan, options, version, stamp)?,
        )?;
        dev.write(
            (plan.integrity + 1) * bs,
            &sealed(zeroed_len(512)?, TAG_TD, version, plan.integrity + 1, 512)?,
        )?;
        partition(&mut dev, &plan, options, version, stamp)?;
        dev.flush()?;
        Self::open(dev.into_inner())
    }
}

/// The volume recognition sequence: `BEA01`, the `NSR` of `version`, `TEA01`.
fn recognition<B: Block>(dev: &mut ByteDevice<B>, version: u16) -> Result<(), DriverError> {
    let stride = u64::from(dev.block_size()).max(2048);
    let nsr: &[u8; 5] = if version == 3 { b"NSR03" } else { b"NSR02" };
    for (index, identifier) in [b"BEA01", nsr, b"TEA01"].into_iter().enumerate() {
        let mut descriptor = [0u8; 7];
        descriptor[1..6].copy_from_slice(identifier);
        descriptor[6] = 1;
        dev.write(VRS_START + index as u64 * stride, &descriptor)?;
    }
    Ok(())
}

/// One volume descriptor sequence starting at sector `start`: primary,
/// implementation use, partition, logical volume, unallocated space and
/// terminating descriptors.
fn descriptors(
    plan: &Plan,
    options: &FormatOptions<'_>,
    version: u16,
    start: u64,
    stamp: [u8; 12],
) -> Result<Vec<Vec<u8>>, DriverError> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Vec::new();
    let serial: Vec<u8> = options
        .serial
        .iter()
        .flat_map(|&byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0xF)]])
        .collect();
    let set = core::str::from_utf8(&serial).map_err(|_| DriverError::OutOfRange)?;
    let mut set_identifier = alloc::string::String::from(set);
    set_identifier.push_str(options.label);

    let mut pvd = zeroed_len(512)?;
    pvd[24..56].copy_from_slice(&dstring(options.label, 32)?);
    pvd[56..58].copy_from_slice(&1u16.to_le_bytes());
    pvd[58..60].copy_from_slice(&1u16.to_le_bytes());
    pvd[60..62].copy_from_slice(&2u16.to_le_bytes());
    pvd[62..64].copy_from_slice(&3u16.to_le_bytes());
    pvd[64..68].copy_from_slice(&1u32.to_le_bytes());
    pvd[68..72].copy_from_slice(&1u32.to_le_bytes());
    let truncated: alloc::string::String = set_identifier.chars().take(63).collect();
    pvd[72..200].copy_from_slice(&dstring(&truncated, 128)?);
    charspec(&mut pvd[200..264]);
    charspec(&mut pvd[264..328]);
    pvd[376..388].copy_from_slice(&stamp);
    regid(&mut pvd[388..420], IMPLEMENTATION, &[]);
    out.push(sealed(pvd, TAG_PVD, version, start, 512)?);

    let mut iuvd = zeroed_len(512)?;
    iuvd[16..20].copy_from_slice(&1u32.to_le_bytes());
    regid(
        &mut iuvd[20..52],
        b"*UDF LV Info",
        &options.revision.to_le_bytes(),
    );
    charspec(&mut iuvd[52..116]);
    iuvd[116..244].copy_from_slice(&dstring(options.label, 128)?);
    regid(&mut iuvd[352..384], IMPLEMENTATION, &[]);
    out.push(sealed(iuvd, TAG_IUVD, version, start + 1, 512)?);

    let mut pd = zeroed_len(512)?;
    pd[16..20].copy_from_slice(&2u32.to_le_bytes());
    pd[20..22].copy_from_slice(&1u16.to_le_bytes());
    regid(
        &mut pd[24..56],
        if version == 3 { b"+NSR03" } else { b"+NSR02" },
        &[],
    );
    // The partition header's unallocated space bitmap, at the partition's
    // first block.
    let bitmap_len = u32_of(HEADER + plan.length.div_ceil(8))?;
    pd[64..68].copy_from_slice(&bitmap_len.to_le_bytes());
    pd[184..188].copy_from_slice(&ACCESS_OVERWRITABLE.to_le_bytes());
    pd[188..192].copy_from_slice(&u32_of(plan.partition)?.to_le_bytes());
    pd[192..196].copy_from_slice(&u32_of(plan.length)?.to_le_bytes());
    regid(&mut pd[196..228], IMPLEMENTATION, &[]);
    out.push(sealed(pd, TAG_PD, version, start + 2, 512)?);

    let mut lvd = zeroed_len(512)?;
    lvd[16..20].copy_from_slice(&3u32.to_le_bytes());
    charspec(&mut lvd[20..84]);
    lvd[84..212].copy_from_slice(&dstring(options.label, 128)?);
    lvd[212..216].copy_from_slice(&u32_of(plan.bs)?.to_le_bytes());
    domain(&mut lvd[216..248], options.revision);
    // The file set descriptor and its terminator.
    lvd[248..252].copy_from_slice(&u32_of(2 * plan.bs)?.to_le_bytes());
    lvd[252..256].copy_from_slice(&u32_of(plan.file_set)?.to_le_bytes());
    lvd[264..268].copy_from_slice(&6u32.to_le_bytes());
    lvd[268..272].copy_from_slice(&1u32.to_le_bytes());
    regid(&mut lvd[272..304], IMPLEMENTATION, &[]);
    lvd[432..436].copy_from_slice(&u32_of(2 * plan.bs)?.to_le_bytes());
    lvd[436..440].copy_from_slice(&u32_of(plan.integrity)?.to_le_bytes());
    lvd[440..446].copy_from_slice(&[1, 6, 1, 0, 0, 0]);
    out.push(sealed(lvd, TAG_LVD, version, start + 3, 446)?);

    let mut usd = zeroed_len(512)?;
    usd[16..20].copy_from_slice(&4u32.to_le_bytes());
    out.push(sealed(usd, TAG_USD, version, start + 4, 24)?);

    out.push(sealed(zeroed_len(512)?, TAG_TD, version, start + 5, 512)?);
    Ok(out)
}

/// The integrity descriptor of a volume just laid out: closed, its free
/// space, and its one directory.
fn integrity(
    plan: &Plan,
    options: &FormatOptions<'_>,
    version: u16,
    stamp: [u8; 12],
) -> Result<Vec<u8>, DriverError> {
    const IMPLEMENTATION_USE: usize = 46;
    let mut lvid = zeroed_len(512)?;
    lvid[16..28].copy_from_slice(&stamp);
    lvid[28..32].copy_from_slice(&1u32.to_le_bytes());
    lvid[40..48].copy_from_slice(&FIRST_UNIQUE_ID.to_le_bytes());
    lvid[72..76].copy_from_slice(&1u32.to_le_bytes());
    lvid[76..80].copy_from_slice(&u32_of(IMPLEMENTATION_USE as u64)?.to_le_bytes());
    lvid[80..84].copy_from_slice(&u32_of(plan.length - plan.used())?.to_le_bytes());
    lvid[84..88].copy_from_slice(&u32_of(plan.length)?.to_le_bytes());
    let iu = 88;
    regid(&mut lvid[iu..iu + 32], IMPLEMENTATION, &[]);
    lvid[iu + 36..iu + 40].copy_from_slice(&1u32.to_le_bytes());
    for (at, revision) in [
        (40, options.revision),
        (42, options.revision),
        (44, options.revision),
    ] {
        lvid[iu + at..iu + at + 2].copy_from_slice(&revision.to_le_bytes());
    }
    sealed(
        lvid,
        TAG_LVID,
        version,
        plan.integrity,
        iu + IMPLEMENTATION_USE,
    )
}

/// The partition: its space bitmap, the file set descriptor and its
/// terminator, and the root directory with its parent record.
fn partition<B: Block>(
    dev: &mut ByteDevice<B>,
    plan: &Plan,
    options: &FormatOptions<'_>,
    version: u16,
    stamp: [u8; 12],
) -> Result<(), DriverError> {
    let bs = plan.bs;
    let at = |block: u64| (plan.partition + block) * bs;
    let bitmap_bytes =
        usize::try_from(plan.bitmap_blocks * bs).map_err(|_| DriverError::NoSpace)?;
    let mut bitmap = zeroed_len(bitmap_bytes)?;
    bitmap[16..20].copy_from_slice(&u32_of(plan.length)?.to_le_bytes());
    bitmap[20..24].copy_from_slice(&u32_of(plan.length.div_ceil(8))?.to_le_bytes());
    let bits = &mut bitmap[usize::try_from(HEADER).unwrap_or(24)..];
    for block in plan.used()..plan.length {
        let block = usize::try_from(block).map_err(|_| DriverError::NoSpace)?;
        bits[block / 8] |= 1 << (block % 8);
    }
    let header = sealed(
        bitmap[..512.min(bitmap.len())].to_vec(),
        TAG_SBD,
        version,
        0,
        24,
    )?;
    bitmap[..TAG_LEN].copy_from_slice(&header[..TAG_LEN]);
    dev.write(at(0), &bitmap)?;

    let mut fsd = zeroed_len(512)?;
    fsd[16..28].copy_from_slice(&stamp);
    fsd[28..30].copy_from_slice(&3u16.to_le_bytes());
    fsd[30..32].copy_from_slice(&3u16.to_le_bytes());
    fsd[32..36].copy_from_slice(&1u32.to_le_bytes());
    fsd[36..40].copy_from_slice(&1u32.to_le_bytes());
    charspec(&mut fsd[48..112]);
    fsd[112..240].copy_from_slice(&dstring(options.label, 128)?);
    charspec(&mut fsd[240..304]);
    fsd[304..336].copy_from_slice(&dstring(options.label, 32)?);
    fsd[400..404].copy_from_slice(&u32_of(bs)?.to_le_bytes());
    fsd[404..408].copy_from_slice(&u32_of(plan.root)?.to_le_bytes());
    domain(&mut fsd[416..448], options.revision);
    dev.write(
        at(plan.file_set),
        &sealed(fsd, TAG_FSD, version, plan.file_set, 512)?,
    )?;
    dev.write(
        at(plan.file_set + 1),
        &sealed(zeroed_len(512)?, TAG_TD, version, plan.file_set + 1, 512)?,
    )?;

    // The root's one record names itself as its parent.
    let fid_len = FID_FIXED.next_multiple_of(4);
    let mut fid = zeroed_len(fid_len)?;
    fid[16..18].copy_from_slice(&1u16.to_le_bytes());
    fid[18] = CHAR_DIRECTORY | CHAR_PARENT;
    fid[20..24].copy_from_slice(&u32_of(bs)?.to_le_bytes());
    fid[24..28].copy_from_slice(&u32_of(plan.root)?.to_le_bytes());
    let mut data = zeroed_len(usize::try_from(bs).map_err(|_| DriverError::NoSpace)?)?;
    data[..fid_len].copy_from_slice(&sealed(fid, TAG_FID, version, plan.root_data, fid_len)?);
    dev.write(at(plan.root_data), &data)?;

    let extended = options.revision >= 0x0200;
    let fixed = if extended { EFE_FIXED } else { FE_FIXED };
    let mut root = zeroed_len(usize::try_from(bs).map_err(|_| DriverError::NoSpace)?)?;
    root[20..22].copy_from_slice(&STRATEGY_4.to_le_bytes());
    root[24..26].copy_from_slice(&1u16.to_le_bytes());
    root[27] = TYPE_DIRECTORY;
    root[36..40].copy_from_slice(&UNSPECIFIED_ID.to_le_bytes());
    root[40..44].copy_from_slice(&UNSPECIFIED_ID.to_le_bytes());
    // Owner may do anything; everyone else may read and search.
    root[44..48].copy_from_slice(&0b11111_00101_00101u32.to_le_bytes());
    root[48..50].copy_from_slice(&1u16.to_le_bytes());
    let size = u64::try_from(fid_len).map_err(|_| DriverError::NoSpace)?;
    root[56..64].copy_from_slice(&size.to_le_bytes());
    let (recorded, stamps, checkpoint, implementation, lengths): (
        usize,
        &[usize],
        usize,
        usize,
        usize,
    ) = if extended {
        root[64..72].copy_from_slice(&size.to_le_bytes());
        (72, &[80, 92, 104, 116], 128, 168, 208)
    } else {
        (64, &[72, 84, 96], 108, 128, 168)
    };
    root[recorded..recorded + 8].copy_from_slice(&1u64.to_le_bytes());
    root[checkpoint..checkpoint + 4].copy_from_slice(&1u32.to_le_bytes());
    for &stamp_at in stamps {
        root[stamp_at..stamp_at + 12].copy_from_slice(&stamp);
    }
    regid(
        &mut root[implementation..implementation + 32],
        IMPLEMENTATION,
        &[],
    );
    root[lengths + 4..lengths + 8].copy_from_slice(&8u32.to_le_bytes());
    root[fixed..fixed + 4].copy_from_slice(&u32_of(size)?.to_le_bytes());
    root[fixed + 4..fixed + 8].copy_from_slice(&u32_of(plan.root_data)?.to_le_bytes());
    let id = if extended { TAG_EFE } else { TAG_FE };
    dev.write(
        at(plan.root),
        &sealed(root, id, version, plan.root, fixed + 8)?,
    )?;
    Ok(())
}
