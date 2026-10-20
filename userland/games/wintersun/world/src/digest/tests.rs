use super::{
    chunk, fold_generated, realm, reference_params, world, Folding, REFERENCE_DIGEST, STEPS,
};
use crate::chunk::ChunkBuild;
use crate::land::{RealmLand, Unkept};
use crate::params::{RealmParams, RealmSpec};
use crate::realm::RealmField;
use tairix_parallel::SERIAL;
use tairix_wintersun_net::value::ChunkCoord;

#[test]
fn the_reference_realm_validates() {
    // `reference_params` falls back to the shipped default if its own
    // constants ever stop validating; this is what would catch that.
    let spec = reference_params().spec();
    assert!(RealmParams::new(spec).is_ok());
    assert_eq!(spec.seed, 0x5748_4954_4552_534E);
    assert_eq!(reference_params(), RealmParams::new(spec).expect("legal"));
}

#[test]
fn the_reference_digest_is_what_every_target_must_produce() {
    // The one constant the cross-architecture claim is staked on. A change
    // here is a change to every realm ever generated, and is written down
    // deliberately rather than pasted from a failure.
    assert_eq!(
        world(reference_params()).expect("solves"),
        REFERENCE_DIGEST,
        "the generator's output moved"
    );
}

/// Folded a stage at a time, the digest is the realm's, and a folding that
/// has finished keeps answering it.
#[test]
fn a_digest_folded_a_stage_at_a_time_is_the_realms() {
    let mut folding = Folding::new(reference_params());
    let mut steps = 1;
    let digest = loop {
        match folding.step().expect("folds") {
            Some(digest) => break digest,
            None => steps += 1,
        }
    };
    assert_eq!(digest, REFERENCE_DIGEST);
    assert_eq!(steps, STEPS, "the realm and each stage are folded apart");
    assert_eq!(folding.step().expect("folded"), Some(REFERENCE_DIGEST));
}

#[test]
fn the_digest_is_stable_across_runs() {
    let params = reference_params();
    assert_eq!(
        world(params).expect("solves"),
        world(params).expect("solves")
    );
}

#[test]
fn a_different_seed_gives_a_different_digest() {
    let mut spec = reference_params().spec();
    spec.seed ^= 1;
    let other = RealmParams::new(spec).expect("legal");
    assert_ne!(world(other).expect("solves"), REFERENCE_DIGEST);
}

/// Each parameter reaches what the realm generates, rather than moving the
/// digest only by being folded into it.
#[test]
fn every_parameter_moves_what_the_realm_generates() {
    let base = reference_params().spec();
    let variations = [
        RealmSpec {
            extent_chunks: 64,
            ..base
        },
        RealmSpec {
            coarse_samples: 128,
            ..base
        },
        // Rounded to a square grid of plates, so one on another grid.
        RealmSpec { plates: 16, ..base },
        RealmSpec {
            ocean_permille: 421,
            ..base
        },
        RealmSpec {
            relief_units: 1501,
            ..base
        },
        RealmSpec {
            north_latitude: 69,
            ..base
        },
        RealmSpec {
            south_latitude: -11,
            ..base
        },
        RealmSpec {
            westerlies: tairix_wintersun_net::value::Facing(0x2000),
            ..base
        },
    ];
    let reference = generated(&RealmField::generate(reference_params()).expect("solves"));
    for spec in variations {
        let field = RealmField::generate(RealmParams::new(spec).expect("legal")).expect("solves");
        assert_ne!(
            generated(&field),
            reference,
            "{spec:?} left the realm untouched"
        );
    }
}

#[test]
fn the_realm_and_chunk_digests_are_pure() {
    let land = reference_land();
    let field = land.field();
    assert_eq!(realm(field), realm(field));

    let coord = ChunkCoord { x: 0, y: 0 };
    let first = ChunkBuild::new(coord)
        .expect("fits")
        .finish(&land, &mut Unkept, &SERIAL)
        .expect("builds");
    let second = ChunkBuild::new(coord)
        .expect("fits")
        .finish(&land, &mut Unkept, &SERIAL)
        .expect("builds");
    assert_eq!(chunk(&first), chunk(&second));
}

#[test]
fn neighbouring_chunks_digest_differently() {
    let land = reference_land();
    let a = ChunkBuild::new(ChunkCoord { x: 0, y: 0 })
        .expect("fits")
        .finish(&land, &mut Unkept, &SERIAL)
        .expect("builds");
    let b = ChunkBuild::new(ChunkCoord { x: 1, y: 0 })
        .expect("fits")
        .finish(&land, &mut Unkept, &SERIAL)
        .expect("builds");
    assert_ne!(chunk(&a), chunk(&b));
}

/// The reference realm's land.
fn reference_land() -> RealmLand {
    let field = RealmField::generate(reference_params()).expect("solves");
    RealmLand::new(field, &SERIAL).expect("its land")
}

/// The digest of what `field` generated, without its parameters.
fn generated(field: &RealmField) -> u64 {
    use core::hash::Hasher;

    let mut hasher = tairix_hash::FastHash::new();
    fold_generated(&mut hasher, field);
    hasher.finish()
}
