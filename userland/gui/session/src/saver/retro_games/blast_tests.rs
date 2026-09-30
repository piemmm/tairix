//! Host tests of the blasts: how long each lasts, where its shards fly and
//! come to rest, and the bound on how many go off at once.

use tairix_rng::NonCryptoRng;
use tairix_util::space::{Frame, Pose, Vec3};
use tairix_wm::Scale;

use super::{Blasts, Burst, Piece, Size, MAX_BLASTS, REST, SHARD_S};
use crate::saver::retro_games::wire::{Camera, HullId, Ink, Models, Stage};
use crate::saver::retro_games::{Moment, Rgb, View};

static INK: Ink = Ink {
    edge: Rgb::new(255.0, 90.0, 80.0),
    face: Rgb::new(20.0, 4.0, 4.0),
};

fn burst(start: f64, size: Size) -> Burst {
    Burst {
        at: Vec3::new(0.0, 0.0, 8.0),
        start,
        size,
        ink: &INK,
    }
}

fn pieces() -> [Piece; 3] {
    let pose = Pose::new(Vec3::new(0.0, 0.0, 8.0), Frame::turned(0.4, 0.0));
    [
        Piece {
            hull: HullId::TankBody,
            face: Some(0),
            pose,
        },
        Piece {
            hull: HullId::TankBody,
            face: Some(3),
            pose,
        },
        Piece {
            hull: HullId::TankTurret,
            face: None,
            pose,
        },
    ]
}

#[test]
fn a_blast_is_let_go_once_it_is_spent() {
    let models = Models::new().expect("the solids");
    let mut rng = NonCryptoRng::seed_from_u64(1);
    let mut blasts = Blasts::new().expect("room");
    blasts.spawn(burst(2.0, Size::Wreck), &pieces(), &models, &mut rng);
    blasts.retire(2.0 + SHARD_S - 0.01);
    assert_eq!(blasts.live.len(), 1);
    blasts.retire(2.0 + SHARD_S);
    assert!(blasts.live.is_empty());
}

/// A full set of blasts gives the room of the one furthest spent to the next.
#[test]
fn the_newest_blast_takes_the_place_of_the_oldest() {
    let models = Models::new().expect("the solids");
    let mut rng = NonCryptoRng::seed_from_u64(2);
    let mut blasts = Blasts::new().expect("room");
    for at in 0..=MAX_BLASTS {
        let start = 0.1 * f64::from(u8::try_from(at).expect("small"));
        blasts.spawn(burst(start, Size::Strike), &[], &models, &mut rng);
    }
    assert_eq!(blasts.live.len(), MAX_BLASTS);
    assert!(
        blasts.live.iter().all(|blast| blast.burst.start > 0.05),
        "the first is gone"
    );
}

/// Every shard flies up from the wreck and comes to rest on the floor, and
/// lies still there while it fades.
#[test]
fn shards_come_to_rest_on_the_floor() {
    let models = Models::new().expect("the solids");
    let mut rng = NonCryptoRng::seed_from_u64(3);
    let mut blasts = Blasts::new().expect("room");
    blasts.spawn(burst(0.0, Size::Wreck), &pieces(), &models, &mut rng);
    let blast = &blasts.live[0];
    assert_eq!(blast.shards.len(), 3);
    for shard in &blast.shards {
        assert!(shard.velocity.y > 0.0, "thrown upwards");
        assert!(
            shard.landing > 0.0 && shard.landing < SHARD_S,
            "lands while it is seen"
        );
        let start = shard.piece.pose.point_to_world(shard.middle);
        let landed = start + super::fall(shard.velocity, shard.landing);
        assert!(
            (landed.y - REST).abs() < 1e-9,
            "at rest on the floor: {}",
            landed.y
        );
    }
    let view = View::new((480, 270), Scale::ONE).expect("a view");
    let staged = |age: f64| {
        let mut stage = Stage::new(&view, 64).expect("a stage");
        stage.reset(Camera::at(&view, Moment::default()));
        blasts.stage(age, &mut stage);
        stage.parts().len()
    };
    assert!(
        staged(0.05) > staged(1.5),
        "the flash, the ring and the sparks go first"
    );
    assert_eq!(staged(-0.1), 0, "nothing before it goes off");
    assert_eq!(staged(SHARD_S + 0.1), 0, "nothing once it is spent");
}

/// A strike on the ground throws no shards and is smaller than a wreck.
#[test]
fn a_strike_is_a_smaller_blast() {
    assert!(Size::Strike.scale() < Size::Wreck.scale());
    let models = Models::new().expect("the solids");
    let mut rng = NonCryptoRng::seed_from_u64(4);
    let mut blasts = Blasts::new().expect("room");
    blasts.spawn(burst(0.0, Size::Strike), &[], &models, &mut rng);
    assert!(blasts.live[0].shards.is_empty());
    assert!(blasts.live[0].sparks.len() < super::MAX_SPARKS);
}
