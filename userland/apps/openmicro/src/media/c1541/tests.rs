use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// A 35-track D64 whose every sector is filled with its track and sector
/// mixed, its BAM carrying the ID `AB`; with error codes if given.
fn image(errors: Option<&[(usize, u8)]>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for track in 1..=35u8 {
        for sector in 0..d64::sectors(track) {
            bytes.extend((0..=255u8).map(|i| i ^ track.wrapping_mul(7) ^ sector.wrapping_mul(13)));
        }
    }
    let bam = (1..18u8)
        .map(|t| usize::from(d64::sectors(t)))
        .sum::<usize>()
        * 256;
    bytes[bam + 0xA2] = b'A';
    bytes[bam + 0xA3] = b'B';
    if let Some(errors) = errors {
        let mut codes = vec![0x01; 683];
        for &(sector, code) in errors {
            codes[sector] = code;
        }
        bytes.extend(codes);
    }
    bytes
}

#[test]
fn a_d64_records_each_track_a_revolution_long_at_its_zone() {
    let surface = d64::decode(&image(None)).expect("a D64");
    let track = |t: usize| surface.track(2 * (t - 1)).expect("recorded");
    assert_eq!(track(1).bytes().len(), 7692);
    assert_eq!(track(18).bytes().len(), 7142);
    assert_eq!(track(25).bytes().len(), 6666);
    assert_eq!(track(35).bytes().len(), 6250);
    assert_eq!(track(35).zone(), 0);
    assert!(surface.track(1).is_none(), "nothing between whole tracks");
    assert!(surface.track(2 * 35).is_none(), "nothing past track 35");
}

#[test]
fn a_d64_comes_back_as_it_went_in() {
    let original = image(None);
    let surface = d64::decode(&original).expect("a D64");
    assert_eq!(surface.encode(), (Format::D64 { tracks: 35 }, original));
}

#[test]
fn a_d64_s_error_codes_are_recorded_as_their_faults() {
    let original = image(Some(&[(5, 0x05)]));
    let surface = d64::decode(&original).expect("a D64 with errors");
    let (format, saved) = surface.encode();
    assert_eq!(format, Format::G64, "a sector that does not decode");
    let reloaded = g64::decode(&saved).expect("its own G64");
    assert_eq!(reloaded.track(0), surface.track(0));
}

#[test]
fn a_g64_comes_back_as_it_went_in() {
    let mut surface = d64::decode(&image(None)).expect("a D64");
    surface
        .track_to_write(3, 3)
        .expect("a half-track")
        .set_bit(10, true);
    let saved = g64::encode(&surface);
    let reloaded = g64::decode(&saved).expect("its own G64");
    assert_eq!(reloaded.tracks(), surface.tracks());
}

#[test]
fn a_bit_written_into_a_gap_still_saves_as_a_d64() {
    let original = image(None);
    let mut surface = d64::decode(&original).expect("a D64");
    let last = surface.track(0).expect("track 1").len() - 1;
    surface
        .track_to_write(0, 3)
        .expect("track 1")
        .set_bit(last, false);
    assert!(surface.is_dirty());
    assert_eq!(surface.encode().1, original);
    assert!(surface.is_dirty(), "encoding is not saving");
    surface.mark_saved();
    assert!(!surface.is_dirty());
}

#[test]
fn a_block_written_into_a_gap_saves_as_a_g64() {
    let mut surface = d64::decode(&image(None)).expect("a D64");
    let track = surface.track_to_write(0, 3).expect("track 1");
    let len = track.len();
    // A SYNC in the gap after the last sector's data, the start of a block
    // a D64 has no room for.
    for bit in len - 100..len - 84 {
        track.set_bit(bit, true);
    }
    assert_eq!(surface.encode().0, Format::G64);
}

#[test]
fn a_bam_whose_id_is_not_its_headers_saves_as_a_g64() {
    let original = image(None);
    let mut surface = d64::decode(&original).expect("a D64");
    let bam = (1..18u8)
        .map(|t| usize::from(d64::sectors(t)))
        .sum::<usize>()
        * 256;
    let mut sectors = original[bam..bam + usize::from(d64::sectors(18)) * 256].to_vec();
    sectors[0xA2..0xA4].copy_from_slice(b"XY");
    let ring = d64::record(18, &sectors, None, *b"AB").expect("track 18 recorded");
    *surface.track_to_write(34, 2).expect("track 18") = ring;
    assert_eq!(
        surface.encode().0,
        Format::G64,
        "a D64 would make every header XY"
    );
}

#[test]
fn a_track_written_at_another_zone_holds_a_revolution_at_its_density() {
    let mut surface = d64::decode(&image(None)).expect("a D64");
    let was = surface.track(0).expect("track 1").clone();
    let respaced = surface.track_to_write(0, 0).expect("track 1");
    assert_eq!(respaced.zone(), 0);
    assert_eq!(respaced.len(), d64::revolution_bytes(0) * 8);
    for cell in (0..respaced.len()).step_by(97) {
        assert_eq!(
            respaced.bit(cell),
            was.bit(cell * was.len() / respaced.len()),
            "what lay at the same angle, cell {cell}"
        );
    }
}

#[test]
fn the_next_set_bit_is_found_round_the_ring() {
    let mut track = Track::new(vec![0; 4], 3).expect("a ring");
    assert_eq!(track.cells_to_set_bit(5), None, "nothing set");
    track.set_bit(3, true);
    assert_eq!(track.cells_to_set_bit(3), Some(0), "its own cell");
    assert_eq!(track.cells_to_set_bit(0), Some(3));
    assert_eq!(track.cells_to_set_bit(4), Some(31), "round past the end");
    track.set_bit(20, true);
    assert_eq!(track.cells_to_set_bit(4), Some(16), "across whole bytes");
    assert_eq!(
        track.cells_to_set_bit(36),
        Some(16),
        "a start past the ring wraps"
    );
}

#[test]
fn a_bit_flipped_in_a_data_block_saves_as_a_g64() {
    let mut surface = d64::decode(&image(None)).expect("a D64");
    // Well inside sector 0's data block.
    let track = surface.track_to_write(0, 3).expect("track 1");
    let bit = track.bit(8 * 100);
    track.set_bit(8 * 100, !bit);
    assert_eq!(surface.encode().0, Format::G64);
}

#[test]
fn malformed_images_are_refused() {
    assert_eq!(d64::decode(&[0; 1000]), Err(DiskError::BadSize));
    assert_eq!(
        g64::decode(b"GCR-1540\0\x54\0\0"),
        Err(DiskError::BadSignature)
    );
    let mut g64 = g64::encode(&Surface::blank());
    g64[9] = 85;
    assert_eq!(
        g64::decode(&g64),
        Err(DiskError::Malformed),
        "too many half-tracks"
    );
    let mut g64 = g64::encode(&d64::decode(&image(None)).expect("a D64"));
    g64[12..16].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
    assert_eq!(
        g64::decode(&g64),
        Err(DiskError::Malformed),
        "a track past the end"
    );
    let mut g64 = g64::encode(&d64::decode(&image(None)).expect("a D64"));
    let first = usize::try_from(u32::from_le_bytes([g64[12], g64[13], g64[14], g64[15]]))
        .expect("an offset");
    g64[first..first + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
    assert_eq!(
        g64::decode(&g64),
        Err(DiskError::Malformed),
        "a track past the longest"
    );
}

#[test]
fn a_rewritten_block_whose_padding_the_write_spoiled_still_saves_as_a_d64() {
    let original = image(None);
    let mut surface = d64::decode(&original).expect("a D64");
    // Sector 0's data block begins 29 bytes into track 1; its last GCR byte
    // carries only the padding after the checksum, which a drive turning its
    // write current off leaves as it may.
    let track = surface.track_to_write(0, 3).expect("track 1");
    for bit in 0..8 {
        track.set_bit((29 + 324) * 8 + bit, true);
    }
    assert_eq!(surface.encode(), (Format::D64 { tracks: 35 }, original));
}

#[test]
fn a_g64_of_every_ring_at_its_longest_is_the_largest_decoded() {
    let tracks = (0..HALF_TRACKS)
        .map(|_| Some(Track::new(vec![0x55; MAX_TRACK_BYTES], 3).expect("a ring")))
        .collect();
    let surface = Surface::new(tracks, Format::G64).expect("a disk");
    let mut image = g64::encode(&surface);
    assert_eq!(image.len(), g64::MAX_IMAGE);
    assert!(g64::decode(&image).is_ok());
    image.push(0);
    assert_eq!(g64::decode(&image), Err(DiskError::Malformed));
}

#[test]
fn a_disk_restores_from_a_snapshot_as_it_was() {
    let mut surface = d64::decode(&image(None)).expect("a D64");
    surface.write_protected = true;
    surface
        .track_to_write(3, 3)
        .expect("a half-track")
        .set_bit(10, true);
    let mut w = crate::snapshot::Writer::new();
    w.put(&surface);
    let body = crate::snapshot::open(&w.finish().expect("fits")).expect("opens");
    assert_eq!(
        crate::snapshot::Reader::new(&body).get::<Surface>(),
        Ok(surface)
    );
}
