//! Deterministic fuzz harness for the virtio-gpu driver's reading of the
//! device's answers — device-written bytes it trusts no further than it checks
//! them: the header's verdict and fence, display information, an EDID, a
//! capset's description and bytes, and a mapping's caching.
//!
//! Held to: no input panics; an answer built from random values reads back
//! exactly those values; an answer cut short of a header says nothing ran,
//! and one cut short of its body is refused — but for a mapping, whose blob is
//! mapped all the same and whose caching is then unknown; and a fence is
//! accepted only as echoed.
//!
//! A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep once from a fresh,
//! logged seed; `cargo xtask fuzz` extends it to a wall-clock budget.

use tairix_abi::driver::RegionCaching;
use tairix_abi::DriverError;
use tairix_drv_display_gpu_virtio::wire::{
    capset, capset_info, display_info, edid, fenced, map_info, ok_nodata, status, CapsetInfo,
    CAPSET_INFO_LEN, CAPSET_MAX, DISPLAY_INFO_LEN, EDID_LEN, EDID_MAX, HEADER_LEN, MAP_INFO_LEN,
};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
///
/// Interpreted, it only samples the decoders' paths: they carry no `unsafe`,
/// and a few dozen rounds reach every one.
const SMOKE_ITERATIONS: u64 = if cfg!(miri) { 64 } else { 20_000 };

const OK_NODATA: u32 = 0x1100;
const OK_CAPSET_INFO: u32 = 0x1102;
const OK_CAPSET: u32 = 0x1103;
const OK_MAP_INFO: u32 = 0x1106;

fn put(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// An answer of type `kind`, `len` bytes, fenced with `fence`.
fn answer(kind: u32, len: usize, fence: u64) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    put(&mut bytes, 0, kind);
    put(&mut bytes, 4, 1);
    bytes[8..16].copy_from_slice(&fence.to_le_bytes());
    bytes
}

/// Every reader over `bytes`: each answers or refuses, and none panics.
fn read_all(bytes: &[u8], fence: u64) {
    let _ = ok_nodata(bytes);
    let _ = status(bytes, OK_MAP_INFO);
    let _ = fenced(bytes, fence);
    let _ = display_info(bytes);
    let _ = edid(bytes);
    let _ = capset_info(bytes);
    let _ = capset(bytes, bytes.len().saturating_sub(HEADER_LEN));
    let _ = map_info(bytes);
}

#[test]
fn every_answer_is_read_fail_closed() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "every_answer_is_read_fail_closed",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let fence = rng.next_u64();
            let noise_len = rng.at_most(EDID_LEN + 64);
            let noise = rng.noise(noise_len);
            read_all(&noise, fence);

            let info = CapsetInfo {
                id: rng.next_u32(),
                max_version: rng.next_u32(),
                max_size: rng.next_u32(),
            };
            let mut described = answer(OK_CAPSET_INFO, CAPSET_INFO_LEN, fence);
            put(&mut described, HEADER_LEN, info.id);
            put(&mut described, HEADER_LEN + 4, info.max_version);
            put(&mut described, HEADER_LEN + 8, info.max_size);
            assert_eq!(fenced(&described, fence), Ok(()));
            assert_eq!(capset_info(&described), Ok(info));
            let cut = rng.below(CAPSET_INFO_LEN - 4);
            assert!(capset_info(&described[..cut]).is_err(), "cut at {cut}");
            assert!(
                fenced(&described, fence.wrapping_add(1 + rng.next_u64() % 1000)).is_err(),
                "another fence"
            );

            let len = 1 + rng.below(CAPSET_MAX);
            let mut carried = answer(OK_CAPSET, HEADER_LEN + len, fence);
            rng.fill(&mut carried[HEADER_LEN..]);
            let body = carried[HEADER_LEN..].to_vec();
            assert_eq!(capset(&carried, len), Ok(&body[..]));
            let short = HEADER_LEN + rng.below(len);
            assert_eq!(
                capset(&carried[..short], len),
                Err(DriverError::DeviceFault),
                "a capset cut short"
            );

            let stated = rng.next_u32();
            let mut mapped = answer(OK_MAP_INFO, MAP_INFO_LEN, fence);
            put(&mut mapped, HEADER_LEN, stated);
            let caching = match stated & 0x0f {
                1 => Some(RegionCaching::WriteBack),
                2 => Some(RegionCaching::Uncached),
                3 => Some(RegionCaching::WriteCombine),
                _ => None,
            };
            assert_eq!(map_info(&mapped), Ok(caching));
            let cut = HEADER_LEN + rng.below(4);
            assert_eq!(map_info(&mapped[..cut]), Ok(None), "its caching unknown");
            let below = rng.below(HEADER_LEN);
            assert_eq!(map_info(&mapped[..below]), Err(DriverError::DeviceFault));

            let header = answer(OK_NODATA, HEADER_LEN, fence);
            assert_eq!(ok_nodata(&header), Ok(()));
            assert_eq!(
                ok_nodata(&header[..rng.below(HEADER_LEN)]),
                Err(DriverError::DeviceFault),
                "a header cut short says nothing ran"
            );

            let size = rng.at_most(EDID_MAX + 64);
            let mut sink = answer(0x1104, EDID_LEN, fence);
            put(&mut sink, HEADER_LEN, u32::try_from(size).expect("small"));
            match edid(&sink) {
                Ok(bytes) => assert_eq!(bytes.len(), size),
                Err(refused) => {
                    assert!(size > EDID_MAX, "{size} refused");
                    assert_eq!(refused, DriverError::DeviceFault);
                }
            }

            let mut scanouts = answer(0x1101, DISPLAY_INFO_LEN, fence);
            rng.fill(&mut scanouts[HEADER_LEN..]);
            assert!(display_info(&scanouts).is_ok());
            assert!(display_info(&scanouts[..rng.below(DISPLAY_INFO_LEN)]).is_err());

            let pristine = [&described[..], &carried[..], &mapped[..], &sink[..]];
            let source = *rng.pick(&pristine);
            let mutated = rng.mutated(source);
            read_all(&mutated, fence);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
