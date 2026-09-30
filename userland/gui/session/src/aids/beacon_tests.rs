use tairix_geometry::Scale;
use tairix_theme::{Accessibility, Contrast, Density, Motion, Theme, Timeline};
use tairix_wm::Halo;

use super::{Beacon, FROM_RADIUS_PX, LAG_NS, MS, RING_NS, SHOWN_NS, STILL_RADIUS_PX, TO_RADIUS_PX};

fn reduced() -> Theme {
    Theme::dark().with_axes(Accessibility {
        contrast: Contrast::Normal,
        density: Density::Normal,
        motion: Motion::Reduced,
    })
}

/// The bands of `halo` in the order they are drawn, leaving out their dark
/// rims — which a faint enough ring loses first.
fn bands(halo: &Halo) -> alloc::vec::Vec<(u32, u8)> {
    halo.rings()
        .iter()
        .filter(|ring| (ring.color.r, ring.color.g, ring.color.b) != (0, 0, 0))
        .map(|ring| (ring.radius, ring.color.a))
        .collect()
}

#[test]
fn nothing_is_drawn_until_it_is_sent_and_nothing_once_it_is_done() {
    let theme = Theme::dark();
    let mut beacon = Beacon::new();
    assert!(beacon.halo(0, &theme, Scale::ONE).rings().is_empty());
    assert_eq!(beacon.next_frame_in(0), None);
    beacon.start(1_000 * MS);
    assert!(!beacon
        .halo(1_010 * MS, &theme, Scale::ONE)
        .rings()
        .is_empty());
    assert!(beacon
        .halo(1_000 * MS + SHOWN_NS, &theme, Scale::ONE)
        .rings()
        .is_empty());
    assert_eq!(
        beacon.next_frame_in(1_000 * MS + SHOWN_NS),
        None,
        "and it asks for nothing more"
    );
}

#[test]
fn the_rings_close_in_on_the_pointer_the_second_a_beat_behind() {
    let theme = Theme::dark();
    let mut beacon = Beacon::new();
    beacon.start(0);
    let early = bands(&beacon.halo(10 * MS, &theme, Scale::ONE));
    assert_eq!(early.len(), 1, "the second ring has not set off");
    assert!(early[0].0 > FROM_RADIUS_PX - 8, "{early:?} starts wide");

    let both = bands(&beacon.halo(LAG_NS + 60 * MS, &theme, Scale::ONE));
    assert_eq!(both.len(), 2);
    assert!(both[0].0 > both[1].0, "the follower is the wider: {both:?}");

    // Past the follower's start the leader is the second band drawn.
    let mut previous = u32::MAX;
    let mut now = LAG_NS + 16 * MS;
    while now < RING_NS - 16 * MS {
        let halo = beacon.halo(now, &theme, Scale::ONE);
        let drawn = bands(&halo);
        let leader = drawn.get(1).copied().expect("the leading ring");
        assert!(leader.0 <= previous, "it only ever closes in");
        previous = leader.0;
        now += 16 * MS;
    }
    assert!(
        previous < TO_RADIUS_PX + 4,
        "it arrives at the pointer: {previous}"
    );
}

#[test]
fn a_ring_fades_in_and_is_gone_as_it_arrives() {
    let theme = Theme::dark();
    let mut beacon = Beacon::new();
    beacon.start(0);
    // Before the follower sets off the leader is the only band; after, it is
    // the second, or gone.
    let leader = |beacon: &mut Beacon, at: u64, index: usize| {
        bands(&beacon.halo(at, &theme, Scale::ONE))
            .get(index)
            .map_or(0, |(_, alpha)| *alpha)
    };
    let appearing = leader(&mut beacon, 5 * MS, 0);
    let midway = leader(&mut beacon, 200 * MS, 1);
    let arriving = leader(&mut beacon, RING_NS - 10 * MS, 1);
    assert!(appearing < midway / 2, "{appearing} as it appears");
    assert_eq!(midway, Theme::dark().palette().accent.a);
    assert!(arriving < midway / 4, "{arriving} as it arrives");
}

#[test]
fn each_band_lies_over_a_wider_dark_rim() {
    let theme = Theme::dark();
    let mut beacon = Beacon::new();
    beacon.start(0);
    let halo = beacon.halo(300 * MS, &theme, Scale::ONE);
    assert_eq!(halo.rings().len(), 4, "both rings, each on its rim");
    for pair in halo.rings().chunks(2) {
        let (rim, band) = (pair[0], pair[1]);
        assert!(rim.radius > band.radius && rim.width > band.width);
        assert!(
            rim.radius - rim.width < band.radius - band.width,
            "it edges both sides"
        );
        assert_eq!((rim.color.r, rim.color.g, rim.color.b), (0, 0, 0));
    }
}

#[test]
fn with_motion_reduced_one_ring_stands_still_and_wakes_once() {
    let theme = reduced();
    let mut beacon = Beacon::new();
    beacon.start(0);
    for at in [0, 200 * MS, SHOWN_NS - MS] {
        assert_eq!(
            bands(&beacon.halo(at, &theme, Scale::ONE)),
            [(STILL_RADIUS_PX, theme.palette().accent.a)]
        );
    }
    assert_eq!(beacon.next_frame_in(100 * MS), Some(SHOWN_NS - 100 * MS));
    assert!(beacon.halo(SHOWN_NS, &theme, Scale::ONE).rings().is_empty());
}

#[test]
fn a_moving_ring_asks_for_every_frame_and_no_sooner() {
    let theme = Theme::dark();
    let mut beacon = Beacon::new();
    beacon.start(0);
    let _ = beacon.halo(0, &theme, Scale::ONE);
    assert_eq!(beacon.next_frame_in(0), Some(Timeline::FRAME_NS));
    assert_eq!(beacon.next_frame_in(SHOWN_NS - MS), Some(MS));
    beacon.stop();
    assert_eq!(beacon.next_frame_in(0), None);
}

#[test]
fn the_rings_are_measured_in_logical_pixels() {
    let theme = Theme::dark();
    let doubled = Scale::from_percent(200).expect("a scale");
    let mut beacon = Beacon::new();
    beacon.start(0);
    let one = bands(&beacon.halo(200 * MS, &theme, Scale::ONE));
    let two = bands(&beacon.halo(200 * MS, &theme, doubled));
    assert_eq!(two[0].0, one[0].0 * 2);
}
