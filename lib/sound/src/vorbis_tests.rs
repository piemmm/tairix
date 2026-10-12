//! The decoder over streams written here, against the specification's own
//! formulas evaluated directly: every floor and residue type, both lookups,
//! coupling, several submaps, the window shapes across block switches, and
//! every header refusal.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "the streams written here keep their values within the fields they fill"
)]

extern crate std;

use std::vec;
use std::vec::Vec;

use super::writer::{
    floor0_three_channels, floor1_stereo, packets, vq_float, Book, FloorSpec, PacketSpec,
    Reference, Spec, Values,
};
use super::{channel_map, Decoder, Identification};
use crate::{DecodeError, DecodeLimits};

/// What the decoder returns for `packets`, interleaved.
fn decode(spec: &Spec, packets: &[Vec<u8>]) -> Vec<f32> {
    let identification = Identification::parse(&spec.identification()).expect("an identification");
    let mut decoder = Decoder::new(&identification, &spec.setup()).expect("a setup");
    let mut out = Vec::new();
    for packet in packets {
        decoder.decode(packet).expect("decodes");
        out.extend_from_slice(decoder.pcm());
    }
    out
}

/// The largest difference from the reference, over its largest magnitude.
fn relative_error(got: &[f32], want: &[f64]) -> f64 {
    assert_eq!(got.len(), want.len(), "sample counts");
    let scale = want
        .iter()
        .fold(1e-12f64, |most, value| most.max(value.abs()));
    got.iter()
        .zip(want)
        .map(|(got, want)| (f64::from(*got) - want).abs())
        .fold(0.0, f64::max)
        / scale
}

fn check(spec: &Spec, packets: &[PacketSpec]) {
    let written: Vec<Vec<u8>> = packets.iter().map(|packet| spec.packet(packet)).collect();
    let got = decode(spec, &written);
    let mut reference = Reference::new(spec);
    for packet in packets {
        reference.block(packet);
    }
    let want = reference.samples();
    let error = relative_error(&got, &want);
    assert!(error < 2e-5, "relative error {error}");
}

#[test]
fn floor_1_residue_2_and_coupling_decode_as_the_specification_states() {
    let spec = floor1_stereo();
    let modes = [1, 1, 0, 0, 0, 1, 1, 0, 1, 1];
    let mut unused = vec![vec![false, false]; modes.len()];
    unused[3] = vec![false, true];
    unused[6] = vec![true, true];
    unused[8] = vec![true, false];
    check(&spec, &packets(&spec, &modes, &unused, 0x5eed));
}

#[test]
fn floor_0_residues_0_and_1_and_two_submaps_decode_as_the_specification_states() {
    let spec = floor0_three_channels();
    let modes = [0, 1, 1, 0, 1, 0, 0, 1];
    let mut unused = vec![vec![false; 3]; modes.len()];
    unused[2] = vec![true, false, false];
    unused[5] = vec![false, true, true];
    check(&spec, &packets(&spec, &modes, &unused, 0xf100));
}

/// A packet whose first bit marks it no audio packet, or whose mode the
/// setup lacks, is discarded: it returns nothing and leaves the overlap as
/// it was.
#[test]
fn a_packet_that_is_not_audio_or_names_no_mode_is_discarded() {
    let spec = floor1_stereo();
    let packets = packets(&spec, &[1, 1, 1], &[], 7);
    let written: Vec<Vec<u8>> = packets.iter().map(|packet| spec.packet(packet)).collect();
    let identification = Identification::parse(&spec.identification()).expect("an identification");
    let mut decoder = Decoder::new(&identification, &spec.setup()).expect("a setup");
    assert_eq!(decoder.decode(&written[0]), Ok(Some(0)));
    assert_eq!(decoder.decode(&[0b1]), Ok(None), "a header packet");
    assert_eq!(decoder.decode(&[]), Ok(None), "an empty packet");
    assert_eq!(
        decoder.decode(&[0b0000_0011]),
        Ok(None),
        "a long block's flags missing"
    );
    assert_eq!(decoder.blocksize(&written[1]), Some(256));
    assert_eq!(decoder.blocksize(&[0b1]), None);
    assert_eq!(decoder.decode(&written[1]), Ok(Some(128)));
    let mut whole = decode(&spec, &written[..2]);
    whole.truncate(256);
    assert_eq!(decoder.pcm(), &whole[..]);
}

/// A packet ending part way through its residue keeps what it read, as the
/// format intends, so a cut packet still decodes.
#[test]
fn a_packet_cut_short_decodes_what_it_holds() {
    let spec = floor1_stereo();
    let packets = packets(&spec, &[1, 1, 1], &[], 9);
    let mut written: Vec<Vec<u8>> = packets.iter().map(|packet| spec.packet(packet)).collect();
    let half = written[1].len() / 2;
    written[1].truncate(half);
    let identification = Identification::parse(&spec.identification()).expect("an identification");
    let mut decoder = Decoder::new(&identification, &spec.setup()).expect("a setup");
    for packet in &written {
        assert!(decoder.decode(packet).expect("decodes").is_some());
    }
}

/// A floor 0 whose filter has a root on a band's own frequency draws an
/// infinite curve there; the packet is refused rather than its samples
/// passed on.
#[test]
fn a_packet_whose_samples_cannot_hold_is_refused() {
    let mut spec = floor0_three_channels();
    spec.books[0].values = Some(Values {
        lattice: false,
        minimum: 0,
        delta: 0,
        value_bits: 1,
        sequence: false,
        multiplicands: vec![0; 32],
    });
    let packets = packets(&spec, &[1, 1], &[], 3);
    let written: Vec<Vec<u8>> = packets.iter().map(|packet| spec.packet(packet)).collect();
    let identification = Identification::parse(&spec.identification()).expect("an identification");
    let mut decoder = Decoder::new(&identification, &spec.setup()).expect("a setup");
    assert_eq!(decoder.decode(&written[0]), Ok(Some(0)));
    assert_eq!(
        decoder.decode(&written[1]),
        Err(DecodeError::VorbisPacketInvalid)
    );
}

#[test]
fn an_identification_header_that_does_not_hold_together_is_refused() {
    let good = floor1_stereo().identification();
    assert!(Identification::parse(&good).is_ok());
    let edits: [(usize, u8); 7] = [
        (7, 1),
        (11, 0),
        (12, 0),
        (28, 0x85),
        (28, 0xE8),
        (28, 0x78),
        (29, 0),
    ];
    for (at, value) in edits {
        let mut bad = good.clone();
        if at == 12 {
            bad[12..16].fill(0);
        } else {
            bad[at] = value;
        }
        assert_eq!(
            Identification::parse(&bad),
            Err(DecodeError::VorbisBadIdentification),
            "byte {at} as {value:#x}"
        );
    }
    assert_eq!(
        Identification::parse(&good[..29]),
        Err(DecodeError::VorbisBadIdentification)
    );
    let mut setup = good.clone();
    setup[0] = 5;
    assert_eq!(
        Identification::parse(&setup),
        Err(DecodeError::VorbisBadIdentification)
    );
}

/// Each way a setup can fail to hold together is refused by name.
#[test]
fn a_setup_that_does_not_hold_together_is_refused() {
    let base = floor1_stereo();
    let identification = Identification::parse(&base.identification()).expect("an identification");
    let refused = |spec: &Spec| Decoder::new(&identification, &spec.setup()).err();
    let mut cases: Vec<(&str, Spec)> = Vec::new();
    let mut spec = base.clone();
    spec.books[0].lengths[0] = 1;
    cases.push(("an overspecified code", spec));
    let mut spec = base.clone();
    spec.books[2].lengths = vec![2, 2, 2];
    cases.push(("an underspecified code", spec));
    let mut spec = base.clone();
    if let Some(row) = spec.residues[0].books.get_mut(1) {
        row[2] = Some(0);
    }
    cases.push(("a residue book with no vectors", spec));
    let mut spec = base.clone();
    spec.residues[0].classifications = 3;
    spec.residues[0].books.push([None; 8]);
    cases.push(("more classwords than the classbook's entries", spec));
    let mut spec = base.clone();
    spec.mappings[0].coupling = vec![(1, 1)];
    cases.push(("a channel coupled to itself", spec));
    let mut spec = base.clone();
    spec.mappings[0].submaps = vec![(1, 0)];
    cases.push(("a floor the setup lacks", spec));
    let mut spec = base.clone();
    spec.modes[1].1 = 1;
    cases.push(("a mapping the setup lacks", spec));
    let mut spec = base.clone();
    if let FloorSpec::One { x, .. } = &mut spec.floors[0] {
        x[1] = 30;
    }
    cases.push(("a floor's posts at one place", spec));
    let mut spec = base.clone();
    if let FloorSpec::One { classes, .. } = &mut spec.floors[0] {
        classes[0].subclass_books[1] = Some(9);
    }
    cases.push(("a floor book the setup lacks", spec));
    for (what, spec) in &cases {
        assert_eq!(refused(spec), Some(DecodeError::VorbisBadSetup), "{what}");
    }
    let mut setup = base.setup();
    let last = setup.len() - 1;
    // The framing bit is the last written, so the highest set in the last
    // byte; the bits above it are padding.
    setup[last] &= !(0x80 >> setup[last].leading_zeros());
    assert_eq!(
        Decoder::new(&identification, &setup).err(),
        Some(DecodeError::VorbisBadSetup),
        "no framing bit"
    );
    let mut setup = base.setup();
    setup[0] = 3;
    assert_eq!(
        Decoder::new(&identification, &setup).err(),
        Some(DecodeError::VorbisBadSetup),
        "not a setup header"
    );
}

/// A setup whose codebooks declare more entries, or expand to more values,
/// than any encoder writes is refused before anything is allocated for it.
#[test]
fn a_setup_past_the_bounds_is_refused() {
    let mut spec = floor1_stereo();
    let wide = Book {
        dimensions: 1,
        lengths: vec![18; 1 << 18],
        ordered: true,
        values: None,
    };
    spec.books.push(wide.clone());
    spec.books.push(wide);
    let identification = Identification::parse(&spec.identification()).expect("an identification");
    assert_eq!(
        Decoder::new(&identification, &spec.setup()).err(),
        Some(DecodeError::VorbisSetupTooLarge)
    );
    let mut spec = floor1_stereo();
    spec.books.push(Book {
        dimensions: 8,
        lengths: vec![16; 1 << 16],
        ordered: true,
        values: Some(Values {
            lattice: true,
            minimum: 0,
            delta: vq_float(1, 0),
            value_bits: 1,
            sequence: false,
            multiplicands: vec![0, 1, 0, 1],
        }),
    });
    assert_eq!(
        Decoder::new(&identification, &spec.setup()).err(),
        Some(DecodeError::VorbisSetupTooLarge)
    );
}

/// Vorbis I's channel orders (section 4.3.9): 6.1's seven with their rear
/// centre among them; past eight, the encoder's own order, refused.
#[test]
fn each_channel_count_takes_the_order_vorbis_lays_it_out_in() {
    use tairix_abi::driver::audio::ChannelPosition::{
        FrontCentre, FrontLeft, FrontRight, LowFrequency, RearCentre, SideLeft, SideRight,
    };
    let limits = DecodeLimits::new(8, 4096, 64);
    let positions = |count| channel_map(count, &limits).map(|map| map.positions().to_vec());
    assert_eq!(
        positions(7),
        Ok(vec![
            FrontLeft,
            FrontCentre,
            FrontRight,
            SideLeft,
            SideRight,
            RearCentre,
            LowFrequency
        ])
    );
    for count in 1..=8 {
        assert_eq!(
            positions(count).map(|map| map.len()),
            Ok(usize::from(count))
        );
    }
    assert_eq!(positions(0), Err(DecodeError::NoChannels));
    assert_eq!(positions(9), Err(DecodeError::ChannelsExceedLimit));
}
